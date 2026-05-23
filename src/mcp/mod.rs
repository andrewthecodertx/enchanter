//! MCP (Model Context Protocol) client — stdio transport.
//!
//! Manages MCP server processes: spawn, discover tools, dispatch calls, shutdown.
//! Supports automatic restart of crashed servers with a configurable retry limit.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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

/// Maximum restart attempts per server before giving up.
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
    #[serde(default)]
    pub input_schema: Value,
}

/// A running MCP server with its stdin/stdout handles and restart tracking.
struct McpServer {
    name: String,
    child: Child,
    stdin: Mutex<tokio::process::ChildStdin>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    next_id: Mutex<u64>,
    tools: Vec<McpToolDef>,
    config: McpServerConfig,
    restart_count: u32,
}

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
            match self.start_server(name, config).await {
                Ok(()) => {}
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

    /// Start a single MCP server, call initialize + tools/list.
    async fn start_server(&mut self, name: &str, config: &McpServerConfig) -> Result<()> {
        let server = self.spawn_and_handshake(name, config).await?;
        let tool_count = server.tools.len();
        self.servers.push(server);

        eprintln!(
            "  {} MCP: {} ({} tools)",
            "⟡".bright_magenta(),
            name.bright_white(),
            tool_count.to_string().bright_white()
        );

        Ok(())
    }

    /// Spawn a process, perform initialize handshake, discover tools.
    /// Returns a fully initialized McpServer ready to accept dispatches.
    async fn spawn_and_handshake(&self, name: &str, config: &McpServerConfig) -> Result<McpServer> {
        // Expand env vars in the env map
        let mut env_vars = HashMap::new();
        for (key, val) in &config.env {
            let expanded = if val.starts_with("${") && val.ends_with('}') {
                let var_name = &val[2..val.len() - 1];
                std::env::var(var_name).unwrap_or(val.clone())
            } else {
                val.clone()
            };
            env_vars.insert(key.clone(), expanded);
        }

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .envs(&env_vars)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        let mut child = cmd.spawn()
            .with_context(|| format!("spawning MCP server '{}': {}", name, config.command))?;

        let stdin = child.stdin.take()
            .context("MCP server stdin not available")?;
        let stdout = child.stdout.take()
            .context("MCP server stdout not available")?;

        let mut server = McpServer {
            name: name.to_string(),
            child,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            next_id: Mutex::new(1),
            tools: Vec::new(),
            config: config.clone(),
            restart_count: 0,
        };

        // Send initialize request (with timeout)
        let init_params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "enchanter",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let init_result = timeout(INIT_TIMEOUT, server.send_request("initialize", Some(init_params)))
            .await
            .with_context(|| format!("MCP server '{}' timed out during initialization ({}s)", name, INIT_TIMEOUT.as_secs()))?
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

        // Send initialized notification (no response expected)
        server.send_notification("notifications/initialized", None).await?;

        // Discover tools (with timeout)
        let tools_result = timeout(INIT_TIMEOUT, server.send_request("tools/list", None))
            .await
            .with_context(|| format!("MCP server '{}' timed out during tools/list ({}s)", name, INIT_TIMEOUT.as_secs()))?
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

    /// Get all MCP tool definitions merged with built-in format.
    /// MCP tools are prefixed as "server_name:tool_name".
    pub fn all_tools_json(&self) -> Vec<Value> {
        let mut tools = Vec::new();
        for server in &self.servers {
            for mcp_tool in &server.tools {
                let prefixed_name = format!("{}:{}", server.name, mcp_tool.name);
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
    /// Returns None if the tool name doesn't match any MCP server.
    /// If the server has crashed, attempts to restart it (up to MAX_RESTARTS).
    pub async fn dispatch(&mut self, full_name: &str, arguments: &Value) -> Option<Result<String>> {
        // Parse "server_name:tool_name"
        let (server_name, tool_name) = full_name.split_once(':')?;

        let server_idx = self.servers.iter().position(|s| s.name == server_name)?;

        // Check if server process has exited — if so, attempt restart
        let has_exited = self.servers[server_idx].child.try_wait().ok().flatten().is_some();
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

        match timeout(DISPATCH_TIMEOUT, server.send_request("tools/call", Some(params))).await {
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
    /// Resets the ID counter and re-performs the full handshake.
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

        // Kill old process if still lingering
        let _ = server.child.kill().await;

        // Brief cooldown before restarting
        tokio::time::sleep(RESTART_COOLDOWN).await;

        let name = server.name.clone();
        let config = server.config.clone();

        // Re-spawn and handshake
        let new_server = self.spawn_and_handshake(&name, &config).await?;

        // Replace the old server entry
        self.servers[idx] = new_server;

        // Reset restart count on successful restart so transient crashes
        // don't accumulate across the whole session
        self.servers[idx].restart_count = 0;

        Ok(())
    }

    /// Gracefully shut down all servers.
    pub async fn shutdown_all(&mut self) {
        for server in &mut self.servers {
            // Best-effort: kill the process. Per the MCP spec, the client
            // closes the connection by terminating the process.
            let _ = server.child.kill().await;
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
        // Check for isError flag on the result
        let is_error = result.get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let texts: Vec<String> = content
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
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

impl McpServer {
    /// Send a JSON-RPC request and wait for the matching response.
    /// Skips any notifications or out-of-order messages.
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

        let request_json = serde_json::to_string(&request)
            .context("serializing JSON-RPC request")?;

        // Write to stdin
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(request_json.as_bytes()).await
                .context("writing to MCP server stdin")?;
            stdin.write_all(b"\n").await
                .context("writing newline to MCP server stdin")?;
            stdin.flush().await
                .context("flushing MCP server stdin")?;
        }

        // Read lines from stdout until we get a response matching our request ID.
        // MCP servers may emit notifications between requests, which we skip.
        let mut stdout = self.stdout.lock().await;
        loop {
            let mut line = String::new();
            let bytes_read = stdout.read_line(&mut line)
                .await
                .context("reading from MCP server stdout")?;

            if bytes_read == 0 {
                // EOF — server closed its stdout, likely crashed
                anyhow::bail!(
                    "MCP server '{}' closed connection (process likely exited)",
                    self.name
                );
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let response: JsonRpcResponse = match serde_json::from_str(trimmed) {
                Ok(r) => r,
                Err(_) => {
                    // Not valid JSON-RPC — skip it (could be a server log line)
                    eprintln!(
                        "{} MCP server '{}' sent non-JSON line, skipping",
                        "Warning:".yellow(),
                        self.name
                    );
                    continue;
                }
            };

            // Check if this is a notification (no id) or a response to a different request
            match response.id {
                Some(resp_id) if resp_id == id => {
                    // This is our response
                    if let Some(error) = response.error {
                        anyhow::bail!(
                            "MCP server '{}' error: [{}] {}",
                            self.name,
                            error.code,
                            error.message
                        );
                    }
                    return Ok(response.result.unwrap_or(Value::Null));
                }
                Some(_) => {
                    // Response for a different request — shouldn't happen in
                    // sequential mode, but skip it
                    continue;
                }
                None => {
                    // Notification — skip it
                    continue;
                }
            }
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params.unwrap_or(Value::Object(serde_json::Map::new()))
        });

        let notification_json = serde_json::to_string(&notification)
            .context("serializing JSON-RPC notification")?;

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(notification_json.as_bytes()).await
            .context("writing notification to MCP server stdin")?;
        stdin.write_all(b"\n").await
            .context("writing newline to MCP server stdin")?;
        stdin.flush().await
            .context("flushing MCP server stdin")?;

        Ok(())
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        // Best-effort kill on drop
        let _ = self.child.start_kill();
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
        assert_eq!(extract_tool_result_text(&result), "MCP tool error: file not found");
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
}