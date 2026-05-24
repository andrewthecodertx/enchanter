//! Session persistence — JSONL conversation logging for replay, recovery, and daemon mode.
//!
//! Sessions are stored as JSONL files in ~/.enchanter/sessions/<id>.jsonl.
//! Each line is a self-contained message object, making the format crash-safe
//! (partial writes don't corrupt the rest of the file).
//!
//! The JSONL append-only format was chosen over alternatives (JSON array, SQLite,
//! protobuf) for three reasons:
//! - Crash-safe: appending a line is atomic on most filesystems; a crash at worst
//!   loses the last line, not the whole file.
//! - Human-readable: you can inspect and grep session files directly.
//! - Stream-friendly: the daemon can tail a session file in real time.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::api::Message;

/// Unique session identifier.
pub type SessionId = String;

/// A single recorded event in a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionEntry {
    /// System prompt message (recorded for replay fidelity).
    #[serde(rename = "system")]
    System { content: String },
    /// User message.
    #[serde(rename = "user")]
    User { content: String },
    /// Assistant text response.
    #[serde(rename = "assistant")]
    Assistant { content: String },
    /// Assistant tool call request.
    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    /// Tool call result.
    #[serde(rename = "tool_result")]
    ToolResult { id: String, content: String },
    /// Metadata about the session (model, timestamp).
    #[serde(rename = "meta")]
    Meta {
        model: String,
        started_at: String,
    },
}

/// Summary of a session file for listing.
#[derive(Debug)]
pub struct SessionMeta {
    pub id: SessionId,
    pub started_at: Option<String>,
    #[allow(dead_code)]
    pub model: Option<String>,
    pub message_count: usize,
    pub file_size: u64,
}

/// Active session handle. Wraps the JSONL file for appending.
pub struct Session {
    id: SessionId,
    file: File,
    path: PathBuf,
    message_count: usize,
}

fn sessions_dir() -> PathBuf {
    crate::home::enchanter_home().join("sessions")
}

impl Session {
    /// Start a new session, creating the JSONL file.
    pub fn new(model: &str) -> Result<Self> {
        let dir = sessions_dir();
        std::fs::create_dir_all(&dir).context("creating sessions directory")?;

        let id = uuid::Uuid::new_v4().to_string();
        let path = dir.join(format!("{}.jsonl", id));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("creating session file {}", path.display()))?;

        let mut session = Self {
            id,
            file,
            path,
            message_count: 0,
        };

        // Write meta entry as the first line
        let meta = SessionEntry::Meta {
            model: model.to_string(),
            started_at: chrono::Local::now().to_rfc3339(),
        };
        session.append_entry(&meta)?;

        Ok(session)
    }

    /// Get the session ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the session file path.
    #[allow(dead_code)]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Number of messages recorded in this session (excluding meta).
    #[allow(dead_code)]
    pub fn message_count(&self) -> usize {
        self.message_count
    }

    /// Append a single session entry. Low-level write.
    fn append_entry(&mut self, entry: &SessionEntry) -> Result<()> {
        let mut line = serde_json::to_string(entry).context("serializing session entry")?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .with_context(|| format!("writing to session file {}", self.path.display()))?;
        self.file
            .flush()
            .with_context(|| format!("flushing session file {}", self.path.display()))?;
        self.message_count += 1;
        Ok(())
    }

    /// Record a message from the conversation.
    pub fn append(&mut self, message: &Message) -> Result<()> {
        let entries = message_to_entries(message);
        for entry in entries {
            self.append_entry(&entry)?;
        }
        Ok(())
    }

    /// Record multiple messages at once.
    #[allow(dead_code)]
    pub fn append_many(&mut self, messages: &[Message]) -> Result<()> {
        for msg in messages {
            self.append(msg)?;
        }
        Ok(())
    }

    /// Load a session from a file, returning all entries.
    pub fn load(id: &str) -> Result<Vec<SessionEntry>> {
        let path = sessions_dir().join(format!("{}.jsonl", id));
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading session file {}", path.display()))?;

        let mut entries = Vec::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let entry: SessionEntry = serde_json::from_str(line)
                .with_context(|| format!("parsing session entry: {}", line))?;
            entries.push(entry);
        }

        Ok(entries)
    }

    /// Convert recorded entries back into conversation messages for replay.
    pub fn entries_to_messages(entries: &[SessionEntry]) -> Vec<Message> {
        let mut messages = Vec::new();
        for entry in entries {
            match entry {
                SessionEntry::System { content } => {
                    messages.push(Message::system(content));
                }
                SessionEntry::User { content } => {
                    messages.push(Message::user(content));
                }
                SessionEntry::Assistant { content } => {
                    messages.push(Message::assistant(content));
                }
                SessionEntry::ToolCall { id, name, arguments } => {
                    let tc = crate::api::ToolCall {
                        id: id.clone(),
                        call_type: "function".to_string(),
                        function: crate::api::ToolCallFunction {
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                    };
                    messages.push(Message::assistant_with_tools(vec![tc], None));
                }
                SessionEntry::ToolResult { id, content } => {
                    messages.push(Message::tool_result(id, content));
                }
                SessionEntry::Meta { .. } => {} // Skip meta entries
            }
        }
        messages
    }

    /// List all sessions, sorted by most recent first (based on started_at).
    pub fn list_all() -> Result<Vec<SessionMeta>> {
        let dir = sessions_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("reading sessions directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }

            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let file_size = metadata.len();

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let mut started_at = None;
            let mut model = None;
            let mut message_count = 0;

            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<SessionEntry>(line) {
                    message_count += 1;
                    if let SessionEntry::Meta {
                        started_at: sa,
                        model: m,
                    } = entry
                    {
                        started_at = Some(sa);
                        model = Some(m);
                    }
                }
            }

            sessions.push(SessionMeta {
                id,
                started_at,
                model,
                message_count,
                file_size,
            });
        }

        sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(sessions)
    }
}

/// Convert a Message into one or more SessionEntry values.
fn message_to_entries(message: &Message) -> Vec<SessionEntry> {
    match message.role.as_str() {
        "system" => vec![SessionEntry::System {
            content: message.content.clone().unwrap_or_default(),
        }],
        "user" => vec![SessionEntry::User {
            content: message.content.clone().unwrap_or_default(),
        }],
        "assistant" => {
            let mut entries = Vec::new();

            // If the message has tool calls, record those
            if let Some(tool_calls) = &message.tool_calls {
                for tc in tool_calls {
                    entries.push(SessionEntry::ToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        arguments: tc.function.arguments.clone(),
                    });
                }
            }

            // If there's also text content, record it
            if let Some(content) = &message.content {
                if !content.is_empty() {
                    entries.push(SessionEntry::Assistant {
                        content: content.clone(),
                    });
                }
            }

            // Edge case: assistant message with no tool calls and no content
            if entries.is_empty() {
                entries.push(SessionEntry::Assistant {
                    content: String::new(),
                });
            }

            entries
        }
        "tool" => vec![SessionEntry::ToolResult {
            id: message.tool_call_id.clone().unwrap_or_default(),
            content: message.content.clone().unwrap_or_default(),
        }],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_test_home() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let home = dir.path().join(".enchanter");
        // SAFETY: test-only, single-threaded for this test scope
        unsafe { std::env::set_var("ENCHANTER_HOME", home.to_string_lossy().to_string()) };
        (dir, home)
    }

    // Note: these tests must not run in parallel since they share
    // the ENCHANTER_HOME env var. Use --test-threads=1 if issues arise.

    #[test]
    fn session_records_messages() {
        let _dir = setup_test_home();
        let mut session = Session::new("test-model").unwrap();

        let system = Message::system("You are helpful.");
        let user = Message::user("hello");
        let assistant = Message::assistant("hi there");

        session.append(&system).unwrap();
        session.append(&user).unwrap();
        session.append(&assistant).unwrap();

        assert!(session.message_count() >= 4); // meta + 3 messages
    }

    #[test]
    fn session_roundtrip() {
        let _dir = setup_test_home();
        let mut session = Session::new("test-model-rt").unwrap();

        let system = Message::system("You are a test.");
        let user = Message::user("what is 2+2?");
        let assistant = Message::assistant("4");

        session.append(&system).unwrap();
        session.append(&user).unwrap();
        session.append(&assistant).unwrap();

        let entries = Session::load(session.id()).unwrap();
        let messages = Session::entries_to_messages(&entries);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[1].content.as_deref(), Some("what is 2+2?"));
        assert_eq!(messages[2].role, "assistant");
    }

    #[test]
    fn session_records_tool_calls() {
        let _dir = setup_test_home();
        let mut session = Session::new("test-model-tc").unwrap();

        let system = Message::system("You are helpful.");
        let user = Message::user("list files");
        let tc = crate::api::ToolCall {
            id: "call_tc_1".to_string(),
            call_type: "function".to_string(),
            function: crate::api::ToolCallFunction {
                name: "list_directory".to_string(),
                arguments: r#"{"path": "."}"#.to_string(),
            },
        };
        let assistant_with_tools = Message::assistant_with_tools(vec![tc], None);
        let tool_result = Message::tool_result("call_tc_1", "3 entries");

        session.append(&system).unwrap();
        session.append(&user).unwrap();
        session.append(&assistant_with_tools).unwrap();
        session.append(&tool_result).unwrap();

        let entries = Session::load(session.id()).unwrap();
        let tool_call_entry = entries.iter().find(|e| {
            matches!(e, SessionEntry::ToolCall { name, .. } if name == "list_directory")
        });
        assert!(tool_call_entry.is_some());

        let tool_result_entry = entries.iter().find(|e| {
            matches!(e, SessionEntry::ToolResult { id, .. } if id == "call_tc_1")
        });
        assert!(tool_result_entry.is_some());
    }

    #[test]
    fn list_sessions_finds_created_session() {
        let _dir = setup_test_home();
        let mut session = Session::new("test-model-list").unwrap();
        let user = Message::user("hello");
        session.append(&user).unwrap();

        let sessions = Session::list_all().unwrap();
        assert!(!sessions.is_empty());

        let found = sessions.iter().find(|s| s.id == session.id());
        assert!(found.is_some());
        assert_eq!(found.unwrap().model.as_deref(), Some("test-model-list"));
    }
}