//! Enchanter daemon — background process that keeps MCP servers warm and
//! accepts requests over a Unix socket.
//!
//! Architecture:
//! ```text
//! ┌─────────────┐       Unix socket        ┌─────────────────┐
//! │  enchanter   │◄────────────────────────►│  enchanterd      │
//! │  (thin CLI)  │   ~/.enchanter/sock      │  (daemon)       │
//! └─────────────┘                           │                 │
//!                                            │  - config       │
//!                                            │  - soul         │
//!                                            │  - memory       │
//!                                            │  - skills       │
//!                                            │  - MCP conns    │
//!                                            │  - system prompt│
//!                                            └─────────────────┘
//! ```
//!
//! The CLI connects, sends a `Request`, and receives a stream of `Event`s
//! as JSONL lines over the socket. This avoids the 3-15 second MCP cold
//! start on every invocation.

use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
#[allow(unused_imports)]
use tokio::sync::mpsc;

use crate::agent::AgentSession;
use crate::config::Config;
use crate::home;
use crate::memory::MemoryStore;
use crate::protocol::{Event, Request};
use crate::skills::SkillsIndex;
use crate::soul::Soul;

/// Default idle timeout in minutes before the daemon shuts down.
const DEFAULT_IDLE_TIMEOUT_MINS: u64 = 10;

/// How often to check the idle timer (seconds).
const IDLE_CHECK_INTERVAL_SECS: u64 = 30;

// ── Socket paths ──────────────────────────────────────────────

/// Path to the Unix domain socket.
pub fn socket_path() -> PathBuf {
    home::enchanter_home().join("sock")
}

/// Path to the PID file.
pub fn pid_path() -> PathBuf {
    home::enchanter_home().join("daemon.pid")
}

// ── Daemon server ──────────────────────────────────────────────

/// Running daemon state.
struct DaemonState {
    started_at: Instant,
    idle_timeout: Duration,
}

/// Start the daemon: initialize agent, listen on Unix socket.
pub async fn run_daemon(idle_timeout_mins: Option<u64>) -> Result<()> {
    let sock_path = socket_path();
    let pid_file = pid_path();

    // Clean up stale socket
    if sock_path.exists() {
        // Try to connect — if it works, another daemon is running
        if let Ok(stream) = tokio::net::UnixStream::connect(&sock_path).await {
            drop(stream);
            anyhow::bail!("Daemon is already running (socket {} is active)", sock_path.display());
        }
        // Stale socket — remove it
        std::fs::remove_file(&sock_path)
            .with_context(|| format!("removing stale socket {}", sock_path.display()))?;
    }

    // Write PID file
    let pid = std::process::id();
    std::fs::write(&pid_file, pid.to_string())
        .with_context(|| format!("writing PID file {}", pid_file.display()))?;

    // Ensure cleanup on drop
    let cleanup_sock = sock_path.clone();
    let cleanup_pid = pid_file.clone();
    let cleanup = move || {
        let _ = std::fs::remove_file(&cleanup_sock);
        let _ = std::fs::remove_file(&cleanup_pid);
    };

    // Initialize agent session
    eprintln!("Loading config, soul, memory, skills...");
    let config = Config::load().context("loading config")?;
    let soul = Soul::load_or_fallback().context("loading soul")?;
    let memory = MemoryStore::load().context("loading memory")?;
    let skills = SkillsIndex::discover().context("discovering skills")?;

    let resolved = config.resolve_default();
    let mut agent = AgentSession::new(
        config,
        soul,
        memory,
        skills,
        resolved,
        false, // streaming
        false, // tools enabled
        None,  // no system override
    )?;

    agent.session.append(&agent.messages[0])?;

    // Cap + summarize memory if needed
    let mem_config = agent.config.memory_config().clone();
    if let Err(e) = agent.memory.manage(&agent.client, &mem_config).await {
        eprintln!("Warning: memory management: {}", e);
    }

    eprintln!("Starting MCP servers...");
    agent.start_mcp().await;

    let model = agent.resolved.model.clone();
    let mcp_servers: Vec<String> = agent.mcp.server_names().into_iter().map(String::from).collect();

    eprintln!("Daemon ready on {}", sock_path.display());
    eprintln!("  Model: {}", model);
    eprintln!("  MCP servers: {:?}", mcp_servers);

    // Set up signal handler for graceful shutdown
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_flag = shutdown.clone();
    ctrlc::set_handler(move || {
        eprintln!("\nReceived SIGINT, shutting down...");
        shutdown_flag.store(true, Ordering::SeqCst);
    }).ok();

    // Listen on Unix socket
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding socket {}", sock_path.display()))?;

    let idle_timeout = Duration::from_secs(
        idle_timeout_mins.unwrap_or(DEFAULT_IDLE_TIMEOUT_MINS) * 60
    );
    let state = DaemonState {
        started_at: Instant::now(),
        idle_timeout,
    };
    let model_name = agent.resolved.model.clone();
    let base_url = agent.resolved.base_url.clone();

    // Main accept loop
    let mut last_activity = Instant::now();

    loop {
        // Check shutdown signal
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        // Check idle timeout
        if last_activity.elapsed() > state.idle_timeout {
            eprintln!("Idle timeout reached ({} min), shutting down.", state.idle_timeout.as_secs() / 60);
            break;
        }

        // Accept with timeout so we can check shutdown/idle periodically
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        last_activity = Instant::now();
                        handle_connection(stream, &mut agent, &state, &model_name, &base_url).await;
                    }
                    Err(e) => {
                        eprintln!("Error accepting connection: {}", e);
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(IDLE_CHECK_INTERVAL_SECS)) => {
                // Just loop around and check shutdown/idle
            }
        }
    }

    // Graceful shutdown
    eprintln!("Shutting down daemon...");
    agent.shutdown_mcp().await;
    cleanup();
    // Generate exit summary
    if agent.config.summarize_on_exit() && crate::summary::should_summarize(&agent.messages) {
        eprintln!("  Generating session summary...");
        match tokio::time::timeout(
            Duration::from_secs(10),
            crate::summary::generate_session_summary(&agent.client, &agent.messages),
        ).await {
            Ok(Ok(summary_text)) if !summary_text.is_empty() => {
                if let Err(e) = agent.memory.add_memory(format!("session_summary\n{}", summary_text)) {
                    eprintln!("Warning: Failed to save summary: {}", e);
                }
            }
            _ => {
                let fallback = crate::summary::fallback_summary(&agent.messages);
                let _ = agent.memory.add_memory(format!("session_summary\n{}", fallback));
            }
        }
    }

    Ok(())
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    agent: &mut AgentSession,
    state: &DaemonState,
    _model_name: &str,
    _base_url: &str,
) {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    // Read one request line
    line.clear();
    match reader.read_line(&mut line).await {
        Ok(0) => return, // EOF
        Ok(_) => {}
        Err(e) => {
            eprintln!("Error reading from client: {}", e);
            return;
        }
    }

    let request = match Request::from_jsonl(line.trim()) {
        Ok(req) => req,
        Err(e) => {
            let err_event = Event::Error {
                message: format!("Invalid request: {}", e),
            };
            if let Ok(jsonl) = err_event.to_jsonl() {
                let _ = writer.write_all(format!("{}\n", jsonl).as_bytes()).await;
            }
            return;
        }
    };

    match request {
        Request::Chat { prompt, model, system: _system, no_stream, no_tools } => {
            // Switch model if requested
            if let Some(new_model) = model {
                if let Err(e) = agent.switch_model(&new_model) {
                    let err_event = Event::Error {
                        message: format!("Failed to switch model: {}", e),
                    };
                    if let Ok(jsonl) = err_event.to_jsonl() {
                        let _ = writer.write_all(format!("{}\n", jsonl).as_bytes()).await;
                    }
                    return;
                }
            }

            // Apply overrides
            let prev_no_stream = agent.no_stream;
            let prev_no_tools = agent.no_tools;
            agent.no_stream = no_stream;
            agent.no_tools = no_tools;

            // Run chat via event-yielding method
            match agent.chat_events(&prompt).await {
                Ok((_result, mut rx)) => {
                    // Forward events to client
                    while let Some(event) = rx.recv().await {
                        if let Ok(jsonl) = event.to_jsonl() {
                            if writer.write_all(format!("{}\n", jsonl).as_bytes()).await.is_err() {
                                break; // Client disconnected
                            }
                        }
                    }
                }
                Err(e) => {
                    let err_event = Event::Error {
                        message: format!("Chat error: {}", e),
                    };
                    if let Ok(jsonl) = err_event.to_jsonl() {
                        let _ = writer.write_all(format!("{}\n", jsonl).as_bytes()).await;
                    }
                }
            }

            // Restore overrides
            agent.no_stream = prev_no_stream;
            agent.no_tools = prev_no_tools;
        }
        Request::Ping => {
            let event = Event::Pong;
            if let Ok(jsonl) = event.to_jsonl() {
                let _ = writer.write_all(format!("{}\n", jsonl).as_bytes()).await;
            }
        }
        Request::Status => {
            let info = agent.info();
            let event = Event::StatusInfo {
                model: info.model.clone(),
                mcp_servers: info.mcp_servers.clone(),
                uptime_secs: state.started_at.elapsed().as_secs(),
            };
            if let Ok(jsonl) = event.to_jsonl() {
                let _ = writer.write_all(format!("{}\n", jsonl).as_bytes()).await;
            }
        }
        Request::Shutdown => {
            // Signal handled in main loop
            let event = Event::Done;
            if let Ok(jsonl) = event.to_jsonl() {
                let _ = writer.write_all(format!("{}\n", jsonl).as_bytes()).await;
            }
            // Set a global shutdown flag — we just send the response and the
            // client disconnects. The idle timer or signal handler will catch it.
            // The client can also just kill the process.
            eprintln!("Shutdown requested via socket.");
            std::process::exit(0);
        }
    }
}

// ── Client functions ────────────────────────────────────────────

/// Try to connect to a running daemon. Returns the UnixStream if successful.
pub async fn connect() -> Result<tokio::net::UnixStream> {
    let sock_path = socket_path();
    tokio::net::UnixStream::connect(&sock_path)
        .await
        .with_context(|| format!("connecting to daemon socket {}", sock_path.display()))
}

/// Send a request to the daemon and collect all response events.
pub async fn send_request(request: &Request) -> Result<Vec<Event>> {
    let mut stream = connect().await?;
    let jsonl = request.to_jsonl()?;
    stream.write_all(format!("{}\n", jsonl).as_bytes()).await
        .context("sending request to daemon")?;
    stream.shutdown().await
        .context("shutting down write side of daemon connection")?;

    let mut events = Vec::new();
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await.context("reading response from daemon")? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match Event::from_jsonl(&line) {
            Ok(event) => events.push(event),
            Err(e) => {
                eprintln!("Warning: failed to parse event from daemon: {} (line: {})", e, line);
            }
        }
    }
    Ok(events)
}

/// Check if the daemon is running by trying to ping it.
pub async fn is_running() -> bool {
    match connect().await {
        Ok(mut stream) => {
            // Try to send a ping
            let ping = Request::Ping;
            if let Ok(jsonl) = ping.to_jsonl() {
                if stream.write_all(format!("{}\n", jsonl).as_bytes()).await.is_ok() {
                    if stream.shutdown().await.is_ok() {
                        // Read response
                        let reader = BufReader::new(stream);
                        let mut lines = reader.lines();
                        if let Ok(Some(line)) = lines.next_line().await {
                            if let Ok(Event::Pong) = Event::from_jsonl(line.trim()) {
                                return true;
                            }
                        }
                    }
                }
            }
            false
        }
        Err(_) => false,
    }
}

/// Start the daemon as a background process. Returns the child PID.
pub fn spawn_daemon(idle_timeout_mins: Option<u64>) -> Result<u32> {
    let exe = std::env::current_exe()
        .context("finding current executable")?;

    // Clean up stale socket/pid files
    let sock = socket_path();
    let pid = pid_path();
    if sock.exists() {
        std::fs::remove_file(&sock).ok();
    }
    if pid.exists() {
        std::fs::remove_file(&pid).ok();
    }

    let mut cmd = std::process::Command::new(exe);
    if let Some(mins) = idle_timeout_mins {
        cmd.arg(format!("--idle-timeout={}", mins));
    }
    cmd.arg("daemon").arg("start");
    // Signal to the child process that it should run in the foreground
    // (i.e., actually become the daemon) rather than spawning another child.
    cmd.env("__ENCHANTER_DAEMON_FOREGROUND", "1");

    let child = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawning daemon process")?;

    Ok(child.id())
}

/// Wait for the daemon socket to become available.
pub async fn wait_for_socket(timeout_secs: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while Instant::now() < deadline {
        if socket_path().exists() {
            // Give it a moment to start listening
            tokio::time::sleep(Duration::from_millis(100)).await;
            if is_running().await {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    anyhow::bail!("Timed out waiting for daemon to start ({}s)", timeout_secs)
}

/// Stop the daemon by sending a Shutdown request.
pub async fn stop_daemon() -> Result<()> {
    match send_request(&Request::Shutdown).await {
        Ok(_) => Ok(()),
        Err(e) => {
            // Try killing by PID
            let pid_file = pid_path();
            if pid_file.exists() {
                let pid_str = std::fs::read_to_string(&pid_file)?;
                let pid: u32 = pid_str.trim().parse()?;
                // SIGTERM
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                // Wait a moment
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

/// Print daemon status information.
pub async fn print_status() -> Result<()> {
    match send_request(&Request::Status).await {
        Ok(events) => {
            for event in events {
                match event {
                    Event::StatusInfo { model, mcp_servers, uptime_secs } => {
                        println!("{}", "═══ DAEMON STATUS ═══".bright_cyan());
                        println!("  Model:       {}", model.bright_white());
                        println!("  MCP servers: {}", mcp_servers.join(", ").bright_white());
                        let mins = uptime_secs / 60;
                        let secs = uptime_secs % 60;
                        println!("  Uptime:      {}m {}s", mins, secs);
                        println!("  PID file:    {}", pid_path().display());
                        println!("  Socket:      {}", socket_path().display());
                    }
                    Event::Error { message } => {
                        eprintln!("{} {}", "Error:".red(), message);
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        Err(e) => {
            println!("{}", "Daemon is not running.".dimmed());
            if socket_path().exists() {
                println!("  (stale socket found: {})", socket_path().display());
            }
            Err(e)
        }
    }
}

/// Run a chat request through the daemon, printing events to stdout.
/// Returns the final text response if available.
pub async fn chat_via_daemon(prompt: &str, model: Option<String>, system: Option<String>, no_stream: bool, no_tools: bool) -> Result<Option<String>> {
    let request = Request::Chat {
        prompt: prompt.to_string(),
        model,
        system,
        no_stream,
        no_tools,
    };

    let mut stream = connect().await?;
    let jsonl = request.to_jsonl()?;
    stream.write_all(format!("{}\n", jsonl).as_bytes()).await
        .context("sending chat request to daemon")?;
    stream.shutdown().await
        .context("shutting down write side of daemon connection")?;

    let mut full_response = String::new();
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await.context("reading response from daemon")? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match Event::from_jsonl(&line) {
            Ok(event) => {
                match event {
                    Event::Content { text } => {
                        print!("{}", text);
                        std::io::stdout().flush().ok();
                        full_response.push_str(&text);
                    }
                    Event::ToolCall { name, arguments, .. } => {
                        let display_args: Value = serde_json::from_str(&arguments).unwrap_or(Value::Null);
                        let short_args: String = display_args.to_string().chars().take(80).collect();
                        println!("\n  {} {}({})", "⚙".dimmed(), name.bright_white(), short_args.dimmed());
                    }
                    Event::ToolResult { content, .. } => {
                        // Optionally show tool output (truncated)
                        if !content.is_empty() {
                            let preview: String = content.lines().take(3).collect::<Vec<_>>().join("\n");
                            let truncated = if content.lines().count() > 3 {
                                format!("{}\n  {}({} more lines)", preview.dimmed(), "↳".dimmed(), content.lines().count() - 3)
                            } else {
                                preview
                            };
                            println!("  {}", truncated);
                        }
                    }
                    Event::Done => {
                        if !full_response.is_empty() {
                            println!(); // Trailing newline after content
                        }
                    }
                    Event::Error { message } => {
                        eprintln!("{} {}", "Error:".red(), message);
                    }
                    _ => {}
                }
            }
            Err(e) => {
                eprintln!("Warning: failed to parse event: {} (line: {})", e, line);
            }
        }
    }

    if full_response.is_empty() {
        Ok(None)
    } else {
        Ok(Some(full_response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_under_home() {
        let path = socket_path();
        assert!(path.to_string_lossy().ends_with("sock"));
        assert!(path.to_string_lossy().contains(".enchanter"));
    }

    #[test]
    fn pid_path_is_under_home() {
        let path = pid_path();
        assert!(path.to_string_lossy().ends_with("daemon.pid"));
        assert!(path.to_string_lossy().contains(".enchanter"));
    }
}