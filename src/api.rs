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
use std::time::Duration;

use crate::config::RetryConfig;

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

/// Token usage as reported by the provider. Some providers omit this,
/// especially on streaming responses; callers should fall back to estimates.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug)]
pub struct ChatResult {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub usage: Option<TokenUsage>,
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
    usage: Option<RawUsage>,
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

/// Wire format for the `usage` object; all fields optional since some
/// providers return partial or missing counts.
#[derive(Debug, Deserialize)]
pub struct RawUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl RawUsage {
    fn to_usage(&self) -> TokenUsage {
        let p = self.prompt_tokens.unwrap_or(0);
        let c = self.completion_tokens.unwrap_or(0);
        TokenUsage {
            prompt_tokens: p,
            completion_tokens: c,
            total_tokens: self.total_tokens.unwrap_or(p + c),
        }
    }
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    choices: Vec<StreamChoice>,
    usage: Option<RawUsage>,
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

// ── Retry/backoff ──────────────────────────────────────────────

/// Which HTTP statuses are worth retrying. 429 (rate limited) and every 5xx
/// are transient server-side conditions; other 4xx client errors (400, 401,
/// 403, 404, 422, …) are not.
fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

/// Whether an error from `client.send()` warrants a retry: network-level
/// failures (connect/read timeouts, failed request dispatch). Non-transient
/// errors (auth, TLS, DNS) return immediately.
fn is_retryable_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect() || err.is_request()
}

/// xorshift64* PRNG for backoff jitter — avoids pulling in a rand dependency
/// just for ±20% noise. Seeded once per thread from the system clock.
fn jitter_rand() -> f64 {
    use std::cell::Cell;
    use std::time::{SystemTime, UNIX_EPOCH};

    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0) as u64;
            x = nanos | 1;
        }
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        s.set(x);
        ((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64) / ((1u64 << 53) as f64)
    })
}

/// Backoff delay (including ±20% jitter) for a retry attempt.
///
/// Exponential: `base_delay_ms * 2^attempt`, capped at `max_delay_ms`.
/// A `Retry-After` header value (seconds) overrides the exponential delay
/// when present (still capped at `max_delay_ms`). Returns a duration within
/// `[expected * 0.8, expected * 1.2]` of the un-jittered delay.
fn compute_backoff_delay(
    attempt: u32,
    base_delay_ms: u64,
    max_delay_ms: u64,
    retry_after: Option<u64>,
) -> Duration {
    let base_ms = match retry_after {
        Some(secs) => secs.saturating_mul(1000).min(max_delay_ms),
        None => base_delay_ms
            .saturating_mul(2u64.saturating_pow(attempt))
            .min(max_delay_ms),
    };
    let scaled = base_ms as f64 * (0.8 + 0.4 * jitter_rand());
    Duration::from_millis(scaled.round() as u64)
}

/// Read the `Retry-After` header (seconds) from a response, if present.
fn retry_after_secs(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}

/// Pause for the computed backoff (the delay already includes jitter).
async fn sleep_backoff(delay: Duration) {
    tokio::time::sleep(delay).await;
}

pub struct LlmClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    extra_headers: Vec<(String, String)>,
}

impl LlmClient {
    #[expect(dead_code)]
    pub fn new(base_url: &str, api_key: Option<&str>, model: &str) -> Self {
        Self::with_headers(base_url, api_key, model, Vec::new())
    }

    pub fn with_headers(
        base_url: &str,
        api_key: Option<&str>,
        model: &str,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client builder should not fail with these settings");
        Self {
            client,
            base_url: base_url.to_string(),
            api_key: api_key.map(|s| s.to_string()),
            model: model.to_string(),
            extra_headers,
        }
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = match &self.api_key {
            Some(key) => req.header("Authorization", format!("Bearer {}", key)),
            None => req,
        };
        // Apply extra headers (prompt caching, provider-specific features).
        for (k, v) in &self.extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        req
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
        retry: &RetryConfig,
        mut on_token: F,
    ) -> Result<ChatResult>
    where
        F: FnMut(&str),
    {
        let url = self.base_url.clone();

        let request = ChatRequest {
            model: &self.model,
            messages,
            stream: true,
            temperature: None,
            tools,
        };

        // Rebuild the RequestBuilder per attempt — it isn't Clone.
        let build_request = || {
            self.client
                .post(&url)
                .header("Content-Type", "application/json")
                .json(&request)
        };

        // Retry loop around the request phase only. Once we have a successful
        // response the stream is consumed below without retries.
        let attempts = retry.max_attempts.max(1);
        let mut last_error: Option<anyhow::Error> = None;
        let response = 'retry: {
            for attempt in 0..attempts {
                let response = match self.apply_auth(build_request()).send().await {
                    Ok(resp) => resp,
                    Err(e) => {
                        let retryable = is_retryable_error(&e);
                        let err = anyhow::Error::new(e).context(format!("connecting to {}", url));
                        if !retryable || attempt + 1 >= attempts {
                            return Err(err);
                        }
                        last_error = Some(err);
                        sleep_backoff(compute_backoff_delay(
                            attempt,
                            retry.base_delay_ms,
                            retry.max_delay_ms,
                            None,
                        ))
                        .await;
                        continue;
                    }
                };

                if !response.status().is_success() {
                    let status = response.status();
                    let retry_after = retry_after_secs(&response);
                    let body = response.text().await.unwrap_or_default();
                    let err = anyhow::anyhow!("API error {}: {}", status, body);
                    if !is_retryable_status(status.as_u16()) || attempt + 1 >= attempts {
                        return Err(err);
                    }
                    last_error = Some(err);
                    sleep_backoff(compute_backoff_delay(
                        attempt,
                        retry.base_delay_ms,
                        retry.max_delay_ms,
                        retry_after,
                    ))
                    .await;
                    continue;
                }

                break 'retry response;
            }
            return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("API request failed")));
        };

        let mut full_content = String::new();
        let mut tool_calls_accum: std::collections::BTreeMap<u64, ToolCallAccum> =
            std::collections::BTreeMap::new();
        let mut streamed_usage: Option<TokenUsage> = None;
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;

        let mut buffer = String::new();
        let mut done = false;

        // Per-chunk timeout: if we don't receive data within this duration,
        // the stream is stalled and we bail rather than hanging forever.
        const STREAM_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

        loop {
            if done {
                break;
            }

            let chunk_opt = match tokio::time::timeout(STREAM_CHUNK_TIMEOUT, stream.next()).await {
                Ok(opt) => opt,
                Err(_) => {
                    // Timeout — log it before bailing so the user can see where the hang was.
                    crate::activity_log::log(crate::activity_log::ActivityEvent::StreamTimeout {
                        model: self.model.clone(),
                        elapsed_secs: STREAM_CHUNK_TIMEOUT.as_secs(),
                    });
                    anyhow::bail!(
                        "stream timed out — no data received for {}s",
                        STREAM_CHUNK_TIMEOUT.as_secs()
                    );
                }
            };

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
                        if let Some(u) = &delta.usage {
                            streamed_usage = Some(u.to_usage());
                        }
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

        // If the provider didn't report usage, estimate it: prompt side from
        // the request messages (~4 chars/token), completion side from what we
        // actually received. Marked as an estimate by the caller context.
        let usage = streamed_usage.or_else(|| {
            if full_content.is_empty() && tool_calls_accum.is_empty() {
                None
            } else {
                let prompt = crate::agent::estimate_messages_tokens(messages);
                let completion = (full_content.len() as u64).div_ceil(4);
                Some(TokenUsage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: prompt + completion,
                })
            }
        });

        let content = if full_content.is_empty() {
            None
        } else {
            Some(full_content)
        };

        Ok(ChatResult {
            content,
            tool_calls,
            usage,
        })
    }

    /// Convenience wrapper: streaming chat that prints tokens to stdout.
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: Option<&Value>,
        retry: &RetryConfig,
    ) -> Result<ChatResult> {
        use std::io::Write;
        let result = self
            .chat_stream_with(messages, tools, retry, |token| {
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
    pub async fn chat(
        &self,
        messages: &[Message],
        tools: Option<&Value>,
        retry: &RetryConfig,
    ) -> Result<ChatResult> {
        let url = self.base_url.clone();

        let request = ChatRequest {
            model: &self.model,
            messages,
            stream: false,
            temperature: None,
            tools,
        };

        // Non-streaming requests get a 5-minute total timeout as a safety net.
        const NON_STREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

        // Rebuild the RequestBuilder per attempt — it isn't Clone.
        let build_request = || {
            self.client
                .post(&url)
                .timeout(NON_STREAM_TIMEOUT)
                .header("Content-Type", "application/json")
                .json(&request)
        };

        // Retry loop around the request phase only (send + status check + error
        // mapping). 429/5xx and network errors back off and retry; 4xx client
        // errors and parse failures return immediately.
        let attempts = retry.max_attempts.max(1);
        let mut last_error: Option<anyhow::Error> = None;
        let response = 'retry: {
            for attempt in 0..attempts {
                let response = match self.apply_auth(build_request()).send().await {
                    Ok(resp) => resp,
                    Err(e) => {
                        let retryable = is_retryable_error(&e);
                        let err = anyhow::Error::new(e).context(format!("connecting to {}", url));
                        if !retryable || attempt + 1 >= attempts {
                            return Err(err);
                        }
                        last_error = Some(err);
                        sleep_backoff(compute_backoff_delay(
                            attempt,
                            retry.base_delay_ms,
                            retry.max_delay_ms,
                            None,
                        ))
                        .await;
                        continue;
                    }
                };

                if !response.status().is_success() {
                    let status = response.status();
                    let retry_after = retry_after_secs(&response);
                    let body = response.text().await.unwrap_or_default();
                    let err = anyhow::anyhow!("API error {}: {}", status, body);
                    if !is_retryable_status(status.as_u16()) || attempt + 1 >= attempts {
                        return Err(err);
                    }
                    last_error = Some(err);
                    sleep_backoff(compute_backoff_delay(
                        attempt,
                        retry.base_delay_ms,
                        retry.max_delay_ms,
                        retry_after,
                    ))
                    .await;
                    continue;
                }

                break 'retry response;
            }
            return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("API request failed")));
        };

        let chat_response: ChatResponse = response.json().await.context("parsing API response")?;

        let choice = chat_response.choices.first();
        let content = choice.and_then(|c| c.message.content.clone());
        let tool_calls = choice.and_then(|c| c.message.tool_calls.clone());
        let usage = chat_response.usage.as_ref().map(RawUsage::to_usage);

        Ok(ChatResult {
            content,
            tool_calls,
            usage,
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

    #[test]
    fn usage_parsing_variants() {
        // Full usage object
        let u: RawUsage = serde_json::from_str(
            r#"{"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}"#,
        )
        .unwrap();
        let t = u.to_usage();
        assert_eq!(
            (t.prompt_tokens, t.completion_tokens, t.total_tokens),
            (10, 5, 15)
        );

        // Missing total -> derived from prompt + completion
        let u: RawUsage =
            serde_json::from_str(r#"{"prompt_tokens": 7, "completion_tokens": 3}"#).unwrap();
        let t = u.to_usage();
        assert_eq!(t.total_tokens, 10);

        // Empty usage object -> all zeros (treated as absent by callers via total==0)
        let u: RawUsage = serde_json::from_str("{}").unwrap();
        let t = u.to_usage();
        assert_eq!(t.total_tokens, 0);

        // ChatResponse without usage field -> None
        let resp: ChatResponse =
            serde_json::from_str(r#"{"choices": [{"message": {"content": "hi"}}]}"#).unwrap();
        assert!(resp.usage.is_none());
    }

    /// Assert a backoff delay is within [expected_ms * 0.8, expected_ms * 1.2]
    /// — the jitter bounds for a ±20% swing around the un-jittered delay.
    fn assert_delay_within(delay: Duration, expected_ms: u64) {
        let actual_ms = delay.as_millis() as u64;
        let lo = (expected_ms as f64 * 0.8) as u64;
        let hi = (expected_ms as f64 * 1.2) as u64;
        assert!(
            (lo..=hi).contains(&actual_ms),
            "delay {actual_ms}ms outside [{lo}, {hi}]ms for expected {expected_ms}ms"
        );
    }

    #[test]
    fn backoff_exponential_growth() {
        // Base 500ms doubles per attempt, capped at 8000ms.
        assert_delay_within(compute_backoff_delay(0, 500, 8000, None), 500);
        assert_delay_within(compute_backoff_delay(1, 500, 8000, None), 1000);
        assert_delay_within(compute_backoff_delay(2, 500, 8000, None), 2000);
        assert_delay_within(compute_backoff_delay(3, 500, 8000, None), 4000);
    }

    #[test]
    fn backoff_caps_at_max_delay() {
        // 500 * 2^5 = 16000ms → capped at max_delay_ms = 8000ms.
        assert_delay_within(compute_backoff_delay(5, 500, 8000, None), 8000);
        assert_delay_within(compute_backoff_delay(10, 500, 8000, None), 8000);
    }

    #[test]
    fn backoff_retry_after_override() {
        // Retry-After (seconds) overrides the exponential delay, still capped
        // at max_delay_ms (8000ms = 8s).
        assert_delay_within(compute_backoff_delay(0, 500, 8000, Some(3)), 3000);
        assert_delay_within(compute_backoff_delay(4, 500, 8000, Some(50)), 8000);
    }

    #[test]
    fn backoff_jitter_bounds() {
        // Jitter must keep every sample within ±20% of the expected delay.
        for _ in 0..1000 {
            assert_delay_within(compute_backoff_delay(0, 1000, 8000, None), 1000);
            assert_delay_within(compute_backoff_delay(2, 500, 8000, None), 2000);
            assert_delay_within(compute_backoff_delay(0, 500, 8000, Some(2)), 2000);
        }
    }

    #[test]
    fn retryable_status_classification() {
        // Retryable: 429 and all 5xx.
        assert!(is_retryable_status(429));
        for code in [500, 502, 503, 504, 529] {
            assert!(is_retryable_status(code), "{code} should be retryable");
        }
        // Not retryable: 2xx/3xx (never an error path) and 4xx client errors.
        for code in [200, 204, 301, 400, 401, 403, 404, 422] {
            assert!(!is_retryable_status(code), "{code} should not be retryable");
        }
    }
}
