//! OpenAI-compatible chat completions client.
//!
//! The streaming SSE parser (data: prefix, [DONE] sentinel, delta accumulation)
//! follows the OpenAI Chat Completions streaming protocol as implemented in
//! OpenCode's provider layer (opencode/packages/opencode/src/provider/provider.ts)
//! which wraps SSE responses with timeout handling. enchanter reimplements the
//! same protocol directly in Rust using reqwest + futures_util::StreamExt.
//!
//! The tool_calls streaming accumulation pattern (index-keyed ToolCallAccum
//! map that merges delta objects into complete tool call structs) mirrors the
//! approach used by OpenCode's SDK integration and hermes-agent's streaming
//! response handler (hermes-agent/run_agent.py _process_streaming_response).
//!
//! The Message/ToolCall/ChatResult type structure follows the OpenAI API shape
//! but was also informed by hermes-agent's message dict convention
//! (hermes-agent/run_agent.py message assembly).

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Message model ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[allow(dead_code)]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_with_tools(tool_calls: Vec<ToolCall>, content: Option<String>) -> Self {
        Self {
            role: "assistant".into(),
            content,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// Whether this assistant message contains tool calls.
    #[allow(dead_code)]
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
    }
}

// ── Tool call types ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// Accumulator for streaming tool call deltas.
#[derive(Debug, Default)]
struct ToolCallAccum {
    id: String,
    call_type: String,
    name: String,
    arguments: String,
}

// ── Result type for chat calls ──────────────────────────────────

#[derive(Debug)]
pub struct ChatResult {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl ChatResult {
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
    }
}

// ── API request/response types ──────────────────────────────────

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a Value>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: DeltaContent,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeltaContent {
    content: Option<String>,
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct DeltaToolCall {
    index: Option<u64>,
    id: Option<String>,
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: Option<DeltaToolCallFunction>,
}

#[derive(Debug, Deserialize)]
struct DeltaToolCallFunction {
    name: Option<String>,
    arguments: Option<String>,
}

// ── Client ─────────────────────────────────────────────────────

pub struct LlmClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
}

impl LlmClient {
    pub fn new(base_url: &str, api_key: Option<&str>, model: &str) -> Self {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client builder should not fail with these settings");
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(|s| s.to_string()),
            model: model.to_string(),
        }
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => req.header("Authorization", format!("Bearer {}", key)),
            None => req,
        }
    }

    /// Streaming chat with tool support. Content tokens are emitted via the
    /// provided callback instead of printing directly — this allows both the
    /// CLI (print to stdout) and the daemon (send over socket) to consume
    /// streaming output.
    ///
    /// The SSE stream parsing (data: prefix, [DONE] sentinel, line buffering)
    /// follows the OpenAI streaming protocol as implemented in OpenCode's
    /// provider layer (opencode/packages/opencode/src/provider/).
    pub async fn chat_stream_with<F>(
        &self,
        messages: &[Message],
        tools: Option<&Value>,
        mut on_token: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(&str),
    {
        let url = format!("{}/chat/completions", self.base_url);

        let request = ChatRequest {
            model: &self.model,
            messages,
            stream: true,
            temperature: None,
            tools,
        };

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request);
        let response = self
            .apply_auth(response)
            .send()
            .await
            .with_context(|| format!("connecting to {}", url))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error {}: {}", status, body);
        }

        let mut full_content = String::new();
        let mut tool_calls_accum: std::collections::BTreeMap<u64, ToolCallAccum> =
            std::collections::BTreeMap::new();
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;

        let mut buffer = String::new();
        let mut done = false;

        // Per-chunk timeout: if we don't receive data within this duration,
        // the stream is stalled and we bail rather than hanging forever.
        const STREAM_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

        loop {
            if done { break; }

            let chunk_opt = tokio::time::timeout(STREAM_CHUNK_TIMEOUT, stream.next())
                .await
                .context("stream timed out — no data received for 120s")?;

            let chunk = match chunk_opt {
                Some(c) => c.context("reading stream chunk")?,
                None => break, // stream ended normally
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].trim().to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        done = true;
                        break;
                    }

                    if let Ok(delta) = serde_json::from_str::<StreamDelta>(data) {
                        for choice in &delta.choices {
                            if let Some(content) = &choice.delta.content {
                                full_content.push_str(content);
                                on_token(content);
                            }

                            // Accumulate tool call deltas — the incremental index-based accumulation
                            // pattern (streaming partial function name/arguments by index) follows
                            // the OpenAI streaming spec as used in OpenCode
                            // (opencode/packages/opencode/src/provider/) and Claude Code.
                            if let Some(tc_deltas) = &choice.delta.tool_calls {
                                for tc_delta in tc_deltas {
                                    let idx = tc_delta.index.unwrap_or(0);
                                    let entry = tool_calls_accum.entry(idx).or_default();

                                    if let Some(id) = &tc_delta.id {
                                        entry.id = id.clone();
                                    }
                                    if let Some(ct) = &tc_delta.call_type {
                                        entry.call_type = ct.clone();
                                    }
                                    if let Some(func) = &tc_delta.function {
                                        if let Some(name) = &func.name {
                                            entry.name = name.clone();
                                        }
                                        if let Some(args) = &func.arguments {
                                            entry.arguments.push_str(args);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Reconstruct tool calls from accumulated deltas
        let tool_calls = if tool_calls_accum.is_empty() {
            None
        } else {
            let mut calls = Vec::new();
            for accum in tool_calls_accum.values() {
                calls.push(ToolCall {
                    id: accum.id.clone(),
                    call_type: accum.call_type.clone(),
                    function: ToolCallFunction {
                        name: accum.name.clone(),
                        arguments: accum.arguments.clone(),
                    },
                });
            }
            Some(calls)
        };

        let content = if full_content.is_empty() {
            None
        } else {
            Some(full_content)
        };

        Ok(ChatResult {
            content,
            tool_calls,
        })
    }

    /// Convenience wrapper: streaming chat that prints tokens to stdout.
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: Option<&Value>,
    ) -> Result<ChatResult> {
        use std::io::Write;
        let result = self
            .chat_stream_with(messages, tools, |token| {
                print!("{}", token);
                std::io::stdout().flush().ok();
            })
            .await?;
        if result.content.is_some() {
            println!();
        }
        Ok(result)
    }

    /// Non-streaming chat.
    pub async fn chat(&self, messages: &[Message], tools: Option<&Value>) -> Result<ChatResult> {
        let url = format!("{}/chat/completions", self.base_url);

        let request = ChatRequest {
            model: &self.model,
            messages,
            stream: false,
            temperature: None,
            tools,
        };

        // Non-streaming requests get a 5-minute total timeout as a safety net.
        const NON_STREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        let req_builder = self
            .client
            .post(&url)
            .timeout(NON_STREAM_TIMEOUT)
            .header("Content-Type", "application/json")
            .json(&request);
        let response = self
            .apply_auth(req_builder)
            .send()
            .await
            .with_context(|| format!("connecting to {}", url))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error {}: {}", status, body);
        }

        let chat_response: ChatResponse = response.json().await.context("parsing API response")?;

        let choice = chat_response.choices.first();
        let content = choice.and_then(|c| c.message.content.clone());
        let tool_calls = choice.and_then(|c| c.message.tool_calls.clone());

        Ok(ChatResult {
            content,
            tool_calls,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_construction() {
        let sys = Message::system("you are helpful");
        assert_eq!(sys.role, "system");
        assert_eq!(sys.content.as_deref(), Some("you are helpful"));

        let usr = Message::user("hello");
        assert_eq!(usr.role, "user");
        assert_eq!(usr.content.as_deref(), Some("hello"));

        let ast = Message::assistant("hi there");
        assert_eq!(ast.role, "assistant");
        assert_eq!(ast.content.as_deref(), Some("hi there"));
    }

    #[test]
    fn tool_result_message() {
        let msg = Message::tool_result("call_123", "file contents here");
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_123"));
        assert_eq!(msg.content.as_deref(), Some("file contents here"));
    }

    #[test]
    fn assistant_with_tools_message() {
        let tc = ToolCall {
            id: "call_abc".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "read_file".into(),
                arguments: r#"{"path":"/tmp/test"}"#.into(),
            },
        };
        let msg = Message::assistant_with_tools(vec![tc.clone()], None);
        assert_eq!(msg.role, "assistant");
        assert!(msg.has_tool_calls());
        assert!(msg.content.is_none());

        let calls = msg.tool_calls.unwrap();
        assert_eq!(calls[0].id, "call_abc");
        assert_eq!(calls[0].function.name, "read_file");
    }

    #[test]
    fn message_serialization_roundtrip() {
        let msg = Message::tool_result("call_1", "result text");
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, "tool");
        assert_eq!(parsed.tool_call_id.as_deref(), Some("call_1"));
    }
}
