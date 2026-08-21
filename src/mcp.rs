//! MCP (Model Context Protocol) client — stdio and HTTP transports.
//!
//! Manages MCP server processes: spawn, discover tools, dispatch calls, shutdown.
//! Supports automatic restart of crashed stdio servers with a configurable retry limit.
//!
//! The JSON-RPC-over-stdio transport (initialize/initialized handshake,
//! tools/list discovery, tools/call dispatch) is reimplemented from the
//! Model Context Protocol specification, with reference to hermes-agent's Python
//! MCP client (hermes-agent/tools/mcp_tool.py), which uses the Python `mcp`
//! SDK with anyio transports for the same protocol. Key differences:
//! - hermes-agent wraps the Python MCP SDK; enchanter writes JSON-RPC frames
//!   directly over tokio pipes.
//! - hermes-agent supports SSE transport and per-server timeout/retry config;
//!   enchanter supports stdio and HTTP Streamable transport.
//! - The stdio server auto-restart pattern (MAX_RESTARTS + RESTART_COOLDOWN)
//!   is original to enchanter, not present in hermes-agent's implementation.
//!
//! The HTTP transport with Mcp-Session-Id header tracking and SSE response
//! parsing follows the MCP Streamable HTTP specification (2025-03-26 revision),
//! consistent with OpenCode's MCP HTTP client
//! (opencode/packages/opencode/src/mcp/index.ts), which uses
//! @modelcontextprotocol/sdk's StreamableHTTPClientTransport. enchanter
//! reimplements the same spec directly with reqwest.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::config::McpServerConfig;

/// Maximum time to wait for an MCP server to respond during initialization.
const INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum time to wait for a tools/call response.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum restart attempts per stdio server before giving up.
const MAX_RESTARTS: u32 = 3;

/// Cooldown after a failed restart before trying again.
const RESTART_COOLDOWN: Duration = Duration::from_secs(2);

/// JSON-RPC request.
#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

/// JSON-RPC response.
#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<u64>,
    result: Option<Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[allow(dead_code)]
    code: i64,
    message: String,
}

/// MCP tool definition from a server's tools/list response.
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

// ── Transport layer ─────────────────────────────────────────────

/// The underlying transport for an MCP server connection.
enum McpTransport {
    Stdio {
        child: Box<Child>,
        stdin: Mutex<tokio::process::ChildStdin>,
        stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    },
    Http {
        client: reqwest::Client,
        url: String,
        session_id: Mutex<Option<String>>,
        headers: Vec<(String, String)>,
    },
}

/// A connected MCP server with its transport and metadata.
struct McpServer {
    name: String,
    transport: McpTransport,
    next_id: Mutex<u64>,
    tools: Vec<McpToolDef>,
    config: McpServerConfig,
    restart_count: u32,
}

// ── Connection lifecycle ────────────────────────────────────────

/// Connect to an MCP server using the appropriate transport.
async fn connect_transport(name: &str, config: &McpServerConfig) -> Result<McpTransport> {
    match config.transport_type() {
        crate::config::McpTransportType::Stdio => connect_stdio(name, config).await,
        crate::config::McpTransportType::Http => connect_http(name, config).await,
    }
}

use crate::config::expand_env;

/// Spawn a stdio process and return handles.
async fn connect_stdio(name: &str, config: &McpServerConfig) -> Result<McpTransport> {
    let command = config.command.as_ref().context(format!(
        "MCP server '{}' requires 'command' for stdio transport",
        name
    ))?;

    let mut env_vars = HashMap::new();
    for (key, val) in &config.env {
        env_vars.insert(
            key.clone(),
            expand_env(&format!("MCP server '{name}' env '{key}'"), val),
        );
    }

    let mut cmd = Command::new(command);
    cmd.args(&config.args)
        .envs(&env_vars)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning MCP server '{}': {}", name, command))?;

    let stdin = child
        .stdin
        .take()
        .context("MCP server stdin not available")?;
    let stdout = child
        .stdout
        .take()
        .context("MCP server stdout not available")?;

    Ok(McpTransport::Stdio {
        child: Box::new(child),
        stdin: Mutex::new(stdin),
        stdout: Mutex::new(BufReader::new(stdout)),
    })
}

/// Prepare an HTTP transport (no persistent connection, just a reqwest client).
async fn connect_http(name: &str, config: &McpServerConfig) -> Result<McpTransport> {
    let url = config.url.as_ref().context(format!(
        "MCP server '{}' requires 'url' for HTTP transport",
        name
    ))?;

    let headers = config
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                expand_env(&format!("MCP server '{name}' header '{k}'"), v),
            )
        })
        .collect::<Vec<_>>();

    Ok(McpTransport::Http {
        client: reqwest::Client::new(),
        url: url.clone(),
        session_id: Mutex::new(None),
        headers,
    })
}

// ── JSON-RPC send ──────────────────────────────────────────────

impl McpTransport {
    /// Send a JSON-RPC request and wait for the matching response.
    async fn send_request(&self, server_name: &str, request: &JsonRpcRequest) -> Result<Value> {
        match self {
            McpTransport::Stdio { stdin, stdout, .. } => {
                send_stdio(server_name, stdin, stdout, request).await
            }
            McpTransport::Http {
                client,
                url,
                session_id,
                headers,
            } => send_http(client, url, session_id, headers, request).await,
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(
        &self,
        server_name: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<()> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params.unwrap_or(Value::Object(serde_json::Map::new()))
        });

        match self {
            McpTransport::Stdio { stdin, .. } => {
                let json = serde_json::to_string(&notification)
                    .context("serializing JSON-RPC notification")?;
                let mut stdin = stdin.lock().await;
                stdin
                    .write_all(json.as_bytes())
                    .await
                    .context("writing notification to MCP server stdin")?;
                stdin
                    .write_all(b"\n")
                    .await
                    .context("writing newline to MCP server stdin")?;
                stdin.flush().await.context("flushing MCP server stdin")?;
                Ok(())
            }
            McpTransport::Http {
                client,
                url,
                session_id,
                headers,
                ..
            } => {
                // HTTP notifications are fire-and-forget POSTs
                let mut req = client
                    .post(url.as_str())
                    .header("Content-Type", "application/json")
                    .header("Accept", "application/json, text/event-stream");

                // Attach session header if we have one
                let sid = session_id.lock().await;
                if let Some(ref sid_val) = *sid {
                    req = req.header("Mcp-Session-Id", sid_val.as_str());
                }

                for (k, v) in headers {
                    req = req.header(k.as_str(), v.as_str());
                }

                let resp = req.json(&notification).send().await.with_context(|| {
                    format!("sending notification to HTTP MCP server '{}'", server_name)
                })?;
                // Notifications may return 202 or 200, both are fine
                let _ = resp;
                Ok(())
            }
        }
    }

    /// Check if this transport's process is still alive (stdio only).
    /// Returns Some(exit_status) if dead, None if alive or HTTP transport.
    fn check_liveness(&mut self) -> Option<std::process::ExitStatus> {
        match self {
            McpTransport::Stdio { child, .. } => child.try_wait().ok().flatten(),
            McpTransport::Http { .. } => None,
        }
    }

    /// Force-terminate the transport. Kills the process for stdio, no-op for HTTP.
    async fn terminate(&mut self) {
        match self {
            McpTransport::Stdio { child, .. } => {
                let _ = child.kill().await;
            }
            McpTransport::Http { .. } => {}
        }
    }
}

/// Send a JSON-RPC request via stdio and read the matching response.
async fn send_stdio(
    server_name: &str,
    stdin: &Mutex<tokio::process::ChildStdin>,
    stdout: &Mutex<BufReader<tokio::process::ChildStdout>>,
    request: &JsonRpcRequest,
) -> Result<Value> {
    let request_json = serde_json::to_string(request).context("serializing JSON-RPC request")?;

    // Write to stdin
    {
        let mut stdin = stdin.lock().await;
        stdin
            .write_all(request_json.as_bytes())
            .await
            .context("writing to MCP server stdin")?;
        stdin
            .write_all(b"\n")
            .await
            .context("writing newline to MCP server stdin")?;
        stdin.flush().await.context("flushing MCP server stdin")?;
    }

    // Read lines from stdout until we get a response matching our request ID.
    let mut stdout = stdout.lock().await;
    loop {
        let mut line = String::new();
        let bytes_read = stdout
            .read_line(&mut line)
            .await
            .context("reading from MCP server stdout")?;

        if bytes_read == 0 {
            anyhow::bail!(
                "MCP server '{}' closed connection (process likely exited)",
                server_name
            );
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response: JsonRpcResponse = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(_) => {
                eprintln!(
                    "{} MCP server '{}' sent non-JSON line, skipping",
                    "Warning:".yellow(),
                    server_name
                );
                continue;
            }
        };

        match response.id {
            Some(resp_id) if resp_id == request.id => {
                if let Some(error) = response.error {
                    anyhow::bail!(
                        "MCP server '{}' error: [{}] {}",
                        server_name,
                        error.code,
                        error.message
                    );
                }
                return Ok(response.result.unwrap_or(Value::Null));
            }
            Some(_) => continue,
            None => continue, // notification — skip
        }
    }
}

/// Send a JSON-RPC request via HTTP and parse the response.
/// Handles both direct JSON responses and SSE-streamed responses.
async fn send_http(
    client: &reqwest::Client,
    url: &str,
    session_id: &Mutex<Option<String>>,
    headers: &[(String, String)],
    request: &JsonRpcRequest,
) -> Result<Value> {
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");

    // Attach session header if we have one
    {
        let sid = session_id.lock().await;
        if let Some(ref sid_value) = *sid {
            req = req.header("Mcp-Session-Id", sid_value.as_str());
        }
    }

    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }

    let resp = timeout(DISPATCH_TIMEOUT, req.json(request).send())
        .await
        .with_context(|| {
            format!(
                "HTTP MCP request timed out ({}s)",
                DISPATCH_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("sending request to HTTP MCP server at {}", url))?;

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    // Store/update session ID from response
    if let Some(sid) = resp.headers().get("mcp-session-id")
        && let Ok(sid_str) = sid.to_str()
    {
        let mut sid_guard = session_id.lock().await;
        *sid_guard = Some(sid_str.to_string());
    }

    if content_type.contains("text/event-stream") {
        // SSE response: parse event stream for our response
        parse_sse_http_response(resp, request.id).await
    } else {
        // Direct JSON response
        let body = resp
            .text()
            .await
            .context("reading HTTP MCP response body")?;

        let response: JsonRpcResponse = serde_json::from_str(body.trim()).with_context(|| {
            format!(
                "parsing HTTP MCP response: {}",
                body.chars().take(200).collect::<String>()
            )
        })?;

        match response.id {
            Some(resp_id) if resp_id == request.id => {
                if let Some(error) = response.error {
                    anyhow::bail!("HTTP MCP server error: [{}] {}", error.code, error.message);
                }
                Ok(response.result.unwrap_or(Value::Null))
            }
            Some(_) => {
                anyhow::bail!("HTTP MCP server returned response with unexpected id");
            }
            None => {
                anyhow::bail!("HTTP MCP server returned a notification instead of a response");
            }
        }
    }
}

/// Parse an SSE response from an HTTP MCP server, looking for the response
/// matching our request ID.
async fn parse_sse_http_response(resp: reqwest::Response, expected_id: u64) -> Result<Value> {
    use futures_util::StreamExt;

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading SSE stream chunk")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline_pos) = buffer.find('\n') {
            let line = buffer[..newline_pos].trim().to_string();
            buffer = buffer[newline_pos + 1..].to_string();

            if let Some(data) = line.strip_prefix("data: ") {
                if data == "[DONE]" {
                    anyhow::bail!("HTTP MCP SSE stream ended without matching response");
                }

                if let Ok(response) = serde_json::from_str::<JsonRpcResponse>(data) {
                    match response.id {
                        Some(resp_id) if resp_id == expected_id => {
                            if let Some(error) = response.error {
                                anyhow::bail!(
                                    "HTTP MCP server error: [{}] {}",
                                    error.code,
                                    error.message
                                );
                            }
                            return Ok(response.result.unwrap_or(Value::Null));
                        }
                        Some(_) => continue, // different request id, skip
                        None => continue,    // notification, skip
                    }
                }
                // Non-JSON data line — skip
            }
        }
    }

    anyhow::bail!(
        "HTTP MCP SSE stream ended without matching response (id={})",
        expected_id
    );
}

// ── McpServer handshake ────────────────────────────────────────

impl McpServer {
    /// Create a new server by connecting and performing the full handshake.
    async fn new(name: &str, config: &McpServerConfig) -> Result<Self> {
        let transport = connect_transport(name, config).await?;

        let mut server = Self {
            name: name.to_string(),
            transport,
            next_id: Mutex::new(1),
            tools: Vec::new(),
            config: config.clone(),
            restart_count: 0,
        };

        // Initialize handshake
        let init_params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "enchanter",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let init_result = timeout(
            INIT_TIMEOUT,
            server.send_request("initialize", Some(init_params)),
        )
        .await
        .with_context(|| {
            format!(
                "MCP server '{}' timed out during initialization ({}s)",
                name,
                INIT_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("MCP server '{}' error during initialization", name))?;

        // Check protocol version compatibility
        if let Some(proto_ver) = init_result.get("protocolVersion").and_then(|v| v.as_str())
            && proto_ver != "2024-11-05"
        {
            eprintln!(
                "{} MCP server '{}' uses protocol version {} (expected 2024-11-05)",
                "Warning:".yellow(),
                name,
                proto_ver
            );
        }

        // Send initialized notification
        server
            .transport
            .send_notification(name, "notifications/initialized", None)
            .await?;

        // Discover tools
        let tools_result = timeout(INIT_TIMEOUT, server.send_request("tools/list", None))
            .await
            .with_context(|| {
                format!(
                    "MCP server '{}' timed out during tools/list ({}s)",
                    name,
                    INIT_TIMEOUT.as_secs()
                )
            })?
            .with_context(|| format!("MCP server '{}' error during tools/list", name))?;

        let tools = match serde_json::from_value::<ToolsListResponse>(tools_result) {
            Ok(resp) => resp.tools,
            Err(e) => {
                eprintln!(
                    "{} MCP server '{}' tools/list parse error: {}",
                    "Warning:".yellow(),
                    name,
                    e
                );
                Vec::new()
            }
        };

        server.tools = tools;
        Ok(server)
    }

    /// Send a JSON-RPC request via this server's transport.
    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = {
            let mut next_id = self.next_id.lock().await;
            let id = *next_id;
            *next_id += 1;
            id
        };

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        self.transport.send_request(&self.name, &request).await
    }

    /// Send a JSON-RPC notification via this server's transport.
    #[allow(dead_code)]
    async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<()> {
        self.transport
            .send_notification(&self.name, method, params)
            .await
    }
}

// ── McpManager ─────────────────────────────────────────────────

/// Manager for all MCP server connections.
pub struct McpManager {
    servers: Vec<McpServer>,
}

impl McpManager {
    /// Create an empty manager.
    pub fn new() -> Self {
        Self {
            servers: Vec::new(),
        }
    }

    /// Spawn all configured MCP servers and discover their tools.
    /// Servers that fail to start are skipped with a warning.
    pub async fn start_all(&mut self, configs: &HashMap<String, McpServerConfig>) {
        for (name, config) in configs {
            match McpServer::new(name, config).await {
                Ok(server) => {
                    let tool_count = server.tools.len();
                    let transport_label = match config.transport_type() {
                        crate::config::McpTransportType::Stdio => "stdio",
                        crate::config::McpTransportType::Http => "http",
                    };
                    eprintln!(
                        "  {} MCP: {} ({}, {} tools)",
                        "⟡".bright_magenta(),
                        name.bright_white(),
                        transport_label,
                        tool_count.to_string().bright_white()
                    );
                    self.servers.push(server);
                }
                Err(e) => {
                    eprintln!(
                        "{} MCP server '{}' failed to start: {}",
                        "Warning:".yellow(),
                        name,
                        e
                    );
                }
            }
        }
    }

    /// Get all MCP tool definitions merged with built-in format.
    /// MCP tools are prefixed as "server_name__tool_name".
    pub fn all_tools_json(&self) -> Vec<Value> {
        let mut tools = Vec::new();
        for server in &self.servers {
            for mcp_tool in &server.tools {
                // NB: Double underscore separator must not appear in server names.
                // MCP tool names may contain single underscores (e.g., generate_blog_image),
                // but split_once("__") correctly handles that since it splits on the first match only.
                let prefixed_name = format!("{}__{}", server.name, mcp_tool.name);
                tools.push(json!({
                    "type": "function",
                    "function": {
                        "name": prefixed_name,
                        "description": mcp_tool.description,
                        "parameters": mcp_tool.input_schema,
                    }
                }));
            }
        }
        tools
    }

    /// Dispatch a tool call to the right MCP server.
    /// For stdio servers, auto-restarts on crash (up to MAX_RESTARTS).
    /// Returns None if the tool name doesn't match any MCP server.
    pub async fn dispatch(&mut self, full_name: &str, arguments: &Value) -> Option<Result<String>> {
        let (server_name, tool_name) = full_name.split_once("__")?;
        let server_idx = self.servers.iter().position(|s| s.name == server_name)?;

        // Check if stdio server has exited — attempt restart
        let has_exited = self.servers[server_idx]
            .transport
            .check_liveness()
            .is_some();
        if has_exited {
            match self.restart_server(server_idx).await {
                Ok(()) => {
                    eprintln!(
                        "  {} MCP server '{}' restarted",
                        "↻".bright_green(),
                        server_name
                    );
                }
                Err(e) => {
                    return Some(Err(anyhow::anyhow!(
                        "MCP server '{}' crashed and could not be restarted (after {} attempts): {}",
                        server_name,
                        MAX_RESTARTS,
                        e
                    )));
                }
            }
        }

        let server = &self.servers[server_idx];

        let params = json!({
            "name": tool_name,
            "arguments": arguments
        });

        match timeout(
            DISPATCH_TIMEOUT,
            server.send_request("tools/call", Some(params)),
        )
        .await
        {
            Ok(Ok(result)) => {
                let text = extract_tool_result_text(&result);
                Some(Ok(text))
            }
            Ok(Err(e)) => Some(Err(e)),
            Err(_) => Some(Err(anyhow::anyhow!(
                "MCP server '{}' timed out responding to '{}' ({}s)",
                server_name,
                tool_name,
                DISPATCH_TIMEOUT.as_secs()
            ))),
        }
    }

    /// Attempt to restart a crashed MCP server.
    async fn restart_server(&mut self, idx: usize) -> Result<()> {
        let server = &mut self.servers[idx];

        if server.restart_count >= MAX_RESTARTS {
            anyhow::bail!(
                "MCP server '{}' has exceeded maximum restart attempts ({})",
                server.name,
                MAX_RESTARTS
            );
        }

        server.restart_count += 1;

        // Terminate old transport
        server.transport.terminate().await;

        // Brief cooldown before restarting (stdio only makes sense, but harmless for http)
        tokio::time::sleep(RESTART_COOLDOWN).await;

        let name = server.name.clone();
        let config = server.config.clone();

        // Re-connect and handshake
        let new_server = McpServer::new(&name, &config).await?;

        self.servers[idx] = new_server;
        // Reset restart count on successful restart
        self.servers[idx].restart_count = 0;

        Ok(())
    }

    /// Gracefully shut down all servers.
    pub async fn shutdown_all(&mut self) {
        for server in &mut self.servers {
            server.transport.terminate().await;
        }
    }

    /// Total number of MCP tools across all servers.
    pub fn total_tool_count(&self) -> usize {
        self.servers.iter().map(|s| s.tools.len()).sum()
    }

    /// Server names that are connected.
    pub fn server_names(&self) -> Vec<&str> {
        self.servers.iter().map(|s| s.name.as_str()).collect()
    }
}

/// Extract text content from an MCP tools/call response.
/// Handles the standard format and respects the isError field.
fn extract_tool_result_text(result: &Value) -> String {
    // Standard format: { content: [{ type: "text", text: "..." }] }
    if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let texts: Vec<String> = content
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect();

        if !texts.is_empty() {
            let text = texts.join("\n");
            if is_error {
                return format!("MCP tool error: {}", text);
            }
            return text;
        }
    }

    // Fallback: return the raw JSON if format is unexpected
    if let Some(text) = result.as_str() {
        return text.to_string();
    }

    serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
}

#[derive(Debug, Deserialize)]
struct ToolsListResponse {
    tools: Vec<McpToolDef>,
}

impl Drop for McpTransport {
    fn drop(&mut self) {
        match self {
            McpTransport::Stdio { child, .. } => {
                let _ = child.start_kill();
            }
            McpTransport::Http { .. } => {}
        }
    }
}

// Import colored for warnings
use colored::Colorize;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_from_standard_response() {
        let result = json!({
            "content": [
                { "type": "text", "text": "hello world" }
            ]
        });
        assert_eq!(extract_tool_result_text(&result), "hello world");
    }

    #[test]
    fn extract_text_from_multi_content() {
        let result = json!({
            "content": [
                { "type": "text", "text": "line 1" },
                { "type": "text", "text": "line 2" }
            ]
        });
        assert_eq!(extract_tool_result_text(&result), "line 1\nline 2");
    }

    #[test]
    fn extract_text_from_error_response() {
        let result = json!({
            "isError": true,
            "content": [
                { "type": "text", "text": "file not found" }
            ]
        });
        assert_eq!(
            extract_tool_result_text(&result),
            "MCP tool error: file not found"
        );
    }

    #[test]
    fn extract_text_from_non_error_with_is_error_false() {
        let result = json!({
            "isError": false,
            "content": [
                { "type": "text", "text": "all good" }
            ]
        });
        assert_eq!(extract_tool_result_text(&result), "all good");
    }

    #[test]
    fn extract_text_fallback_raw_json() {
        let result = json!({"some": "value"});
        let text = extract_tool_result_text(&result);
        assert!(text.contains("some"));
    }

    #[test]
    fn manager_new_is_empty() {
        let mgr = McpManager::new();
        assert_eq!(mgr.total_tool_count(), 0);
        assert!(mgr.server_names().is_empty());
        assert!(mgr.all_tools_json().is_empty());
    }

    #[test]
    fn config_transport_type_stdio() {
        let config = McpServerConfig {
            command: Some("npx".to_string()),
            args: vec![],
            env: HashMap::new(),
            url: None,
            headers: HashMap::new(),
        };
        assert_eq!(
            config.transport_type(),
            crate::config::McpTransportType::Stdio
        );
    }

    #[test]
    fn config_transport_type_http() {
        let config = McpServerConfig {
            command: None,
            args: vec![],
            env: HashMap::new(),
            url: Some("https://mcp.example.com/api".to_string()),
            headers: HashMap::new(),
        };
        assert_eq!(
            config.transport_type(),
            crate::config::McpTransportType::Http
        );
    }

    #[test]
    fn config_transport_type_url_takes_precedence() {
        // If both are set, url wins
        let config = McpServerConfig {
            command: Some("npx".to_string()),
            args: vec![],
            env: HashMap::new(),
            url: Some("https://mcp.example.com/api".to_string()),
            headers: HashMap::new(),
        };
        assert_eq!(
            config.transport_type(),
            crate::config::McpTransportType::Http
        );
    }
}
