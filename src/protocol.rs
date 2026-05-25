//! Socket protocol types for enchanter daemon communication.
//!
//! The daemon listens on a Unix socket (`~/.enchanter/sock`). Clients send
//! `Request` messages and receive a stream of `Event` messages, one JSONL
//! line per event. This keeps parsing simple and enables streaming — the
//! client can print content tokens as they arrive.

use serde::{Deserialize, Serialize};

// ── Request types ──────────────────────────────────────────────

/// A request from the CLI client to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Send a chat prompt and receive a streamed response.
    Chat {
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        system: Option<String>,
        #[serde(default)]
        no_stream: bool,
        #[serde(default)]
        no_tools: bool,
    },
    /// Health check — daemon replies with Pong.
    Ping,
    /// Request daemon status info.
    Status,
    /// Ask the daemon to shut down gracefully.
    Shutdown,
}

// ── Event types ────────────────────────────────────────────────

/// A streaming event from the daemon to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A chunk of assistant text content.
    Content { text: String },
    /// The model is making a tool call.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    /// A tool call has completed with a result.
    ToolResult {
        id: String,
        content: String,
    },
    /// The response is complete.
    Done,
    /// Response to a Ping.
    Pong,
    /// Daemon status info.
    StatusInfo {
        model: String,
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        mcp_servers: Vec<String>,
        uptime_secs: u64,
    },
    /// An error occurred.
    Error { message: String },
}

// ── Serialization helpers ──────────────────────────────────────

impl Request {
    /// Serialize this request as a single JSONL line.
    pub fn to_jsonl(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Deserialize a request from a JSONL line.
    pub fn from_jsonl(line: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(line)?)
    }
}

impl Event {
    /// Serialize this event as a single JSONL line.
    pub fn to_jsonl(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Deserialize an event from a JSONL line.
    pub fn from_jsonl(line: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(line)?)
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_roundtrip() {
        let req = Request::Chat {
            prompt: "explain ownership".into(),
            model: Some("gpt-4.1".into()),
            system: None,
            no_stream: false,
            no_tools: false,
        };
        let jsonl = req.to_jsonl().unwrap();
        let decoded = Request::from_jsonl(&jsonl).unwrap();
        assert_eq!(jsonl, r#"{"type":"chat","prompt":"explain ownership","model":"gpt-4.1","no_stream":false,"no_tools":false}"#);
        match decoded {
            Request::Chat { prompt, model, no_stream, no_tools, .. } => {
                assert_eq!(prompt, "explain ownership");
                assert_eq!(model, Some("gpt-4.1".into()));
                assert!(!no_stream);
                assert!(!no_tools);
            }
            _ => panic!("expected Chat variant"),
        }
    }

    #[test]
    fn chat_request_minimal_roundtrip() {
        let req = Request::Chat {
            prompt: "hello".into(),
            model: None,
            system: None,
            no_stream: false,
            no_tools: false,
        };
        let jsonl = req.to_jsonl().unwrap();
        let decoded = Request::from_jsonl(&jsonl).unwrap();
        assert_eq!(jsonl, r#"{"type":"chat","prompt":"hello","no_stream":false,"no_tools":false}"#);
        match decoded {
            Request::Chat { prompt, model, system, no_stream, no_tools } => {
                assert_eq!(prompt, "hello");
                assert!(model.is_none());
                assert!(system.is_none());
                assert!(!no_stream);
                assert!(!no_tools);
            }
            _ => panic!("expected Chat variant"),
        }
    }

    #[test]
    fn ping_request_roundtrip() {
        let req = Request::Ping;
        assert_eq!(req.to_jsonl().unwrap(), r#"{"type":"ping"}"#);
        let decoded = Request::from_jsonl(r#"{"type":"ping"}"#).unwrap();
        assert!(matches!(decoded, Request::Ping));
    }

    #[test]
    fn status_request_roundtrip() {
        let req = Request::Status;
        assert_eq!(req.to_jsonl().unwrap(), r#"{"type":"status"}"#);
        let decoded = Request::from_jsonl(r#"{"type":"status"}"#).unwrap();
        assert!(matches!(decoded, Request::Status));
    }

    #[test]
    fn shutdown_request_roundtrip() {
        let req = Request::Shutdown;
        assert_eq!(req.to_jsonl().unwrap(), r#"{"type":"shutdown"}"#);
        let decoded = Request::from_jsonl(r#"{"type":"shutdown"}"#).unwrap();
        assert!(matches!(decoded, Request::Shutdown));
    }

    #[test]
    fn content_event_roundtrip() {
        let event = Event::Content { text: "Ownership is...".into() };
        let jsonl = event.to_jsonl().unwrap();
        let decoded = Event::from_jsonl(&jsonl).unwrap();
        assert!(jsonl.contains(r#""type":"content""#));
        assert!(jsonl.contains(r#""text":"Ownership is...""#));
        match decoded {
            Event::Content { text } => assert_eq!(text, "Ownership is..."),
            _ => panic!("expected Content variant"),
        }
    }

    #[test]
    fn tool_call_event_roundtrip() {
        let event = Event::ToolCall {
            id: "call_1".into(),
            name: "exec_command".into(),
            arguments: r#"{"command":"ls"}"#.into(),
        };
        let jsonl = event.to_jsonl().unwrap();
        let decoded = Event::from_jsonl(&jsonl).unwrap();
        match decoded {
            Event::ToolCall { id, name, arguments } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "exec_command");
                assert_eq!(arguments, r#"{"command":"ls"}"#);
            }
            _ => panic!("expected ToolCall variant"),
        }
    }

    #[test]
    fn tool_result_event_roundtrip() {
        let event = Event::ToolResult {
            id: "call_1".into(),
            content: "file1.txt\nfile2.txt".into(),
        };
        let jsonl = event.to_jsonl().unwrap();
        let decoded = Event::from_jsonl(&jsonl).unwrap();
        match decoded {
            Event::ToolResult { id, content } => {
                assert_eq!(id, "call_1");
                assert_eq!(content, "file1.txt\nfile2.txt");
            }
            _ => panic!("expected ToolResult variant"),
        }
    }

    #[test]
    fn done_event_roundtrip() {
        let event = Event::Done;
        assert_eq!(event.to_jsonl().unwrap(), r#"{"type":"done"}"#);
        let decoded = Event::from_jsonl(r#"{"type":"done"}"#).unwrap();
        assert!(matches!(decoded, Event::Done));
    }

    #[test]
    fn pong_event_roundtrip() {
        let event = Event::Pong;
        assert_eq!(event.to_jsonl().unwrap(), r#"{"type":"pong"}"#);
        let decoded = Event::from_jsonl(r#"{"type":"pong"}"#).unwrap();
        assert!(matches!(decoded, Event::Pong));
    }

    #[test]
    fn status_info_event_roundtrip() {
        let event = Event::StatusInfo {
            model: "gpt-4.1".into(),
            mcp_servers: vec!["github".into(), "images".into()],
            uptime_secs: 3600,
        };
        let jsonl = event.to_jsonl().unwrap();
        let decoded = Event::from_jsonl(&jsonl).unwrap();
        match decoded {
            Event::StatusInfo { model, mcp_servers, uptime_secs } => {
                assert_eq!(model, "gpt-4.1");
                assert_eq!(mcp_servers, vec!["github", "images"]);
                assert_eq!(uptime_secs, 3600);
            }
            _ => panic!("expected StatusInfo variant"),
        }
    }

    #[test]
    fn status_info_event_empty_servers_roundtrip() {
        let event = Event::StatusInfo {
            model: "claude-3".into(),
            mcp_servers: vec![],
            uptime_secs: 0,
        };
        let jsonl = event.to_jsonl().unwrap();
        // Empty mcp_servers should be omitted due to skip_serializing_if
        assert!(!jsonl.contains("mcp_servers"));
        let decoded = Event::from_jsonl(&jsonl).unwrap();
        match decoded {
            Event::StatusInfo { model, mcp_servers, uptime_secs } => {
                assert_eq!(model, "claude-3");
                assert!(mcp_servers.is_empty());
                assert_eq!(uptime_secs, 0);
            }
            _ => panic!("expected StatusInfo variant"),
        }
    }

    #[test]
    fn error_event_roundtrip() {
        let event = Event::Error { message: "connection refused".into() };
        let jsonl = event.to_jsonl().unwrap();
        let decoded = Event::from_jsonl(&jsonl).unwrap();
        match decoded {
            Event::Error { message } => assert_eq!(message, "connection refused"),
            _ => panic!("expected Error variant"),
        }
    }

    #[test]
    fn all_request_types_from_raw_json() {
        let cases = [
            (r#"{"type":"chat","prompt":"hi","no_stream":true,"no_tools":false}"#, "chat"),
            (r#"{"type":"ping"}"#, "ping"),
            (r#"{"type":"status"}"#, "status"),
            (r#"{"type":"shutdown"}"#, "shutdown"),
        ];
        for (json, label) in cases {
            let req = Request::from_jsonl(json).unwrap();
            match label {
                "chat" => assert!(matches!(req, Request::Chat { .. })),
                "ping" => assert!(matches!(req, Request::Ping)),
                "status" => assert!(matches!(req, Request::Status)),
                "shutdown" => assert!(matches!(req, Request::Shutdown)),
                _ => panic!("unknown label"),
            }
        }
    }

    #[test]
    fn all_event_types_from_raw_json() {
        let cases: Vec<(&str, &str)> = vec![
            (r#"{"type":"content","text":"hello"}"#, "content"),
            (r#"{"type":"tool_call","id":"1","name":"ls","arguments":"{}"}"#, "tool_call"),
            (r#"{"type":"tool_result","id":"1","content":"ok"}"#, "tool_result"),
            (r#"{"type":"done"}"#, "done"),
            (r#"{"type":"pong"}"#, "pong"),
            (r#"{"type":"status_info","model":"gpt-4.1","uptime_secs":100}"#, "status_info"),
            (r#"{"type":"error","message":"fail"}"#, "error"),
        ];
        for (json, label) in cases {
            let evt = Event::from_jsonl(json).unwrap();
            match label {
                "content" => assert!(matches!(evt, Event::Content { .. })),
                "tool_call" => assert!(matches!(evt, Event::ToolCall { .. })),
                "tool_result" => assert!(matches!(evt, Event::ToolResult { .. })),
                "done" => assert!(matches!(evt, Event::Done)),
                "pong" => assert!(matches!(evt, Event::Pong)),
                "status_info" => assert!(matches!(evt, Event::StatusInfo { .. })),
                "error" => assert!(matches!(evt, Event::Error { .. })),
                _ => panic!("unknown label"),
            }
        }
    }
}