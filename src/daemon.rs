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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
#[allow(unused_imports)]
use tokio::sync::mpsc;

use crate::agent::{AgentSession, SessionOptions};
use crate::home;
use crate::protocol::{Event, Request};

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
            anyhow::bail!(
                "Daemon is already running (socket {} is active)",
                sock_path.display()
            );
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
    eprintln!("Loading config, soul, memory, knowledge, skills...");
    let overlay = std::env::current_dir()
        .ok()
        .as_ref()
        .and_then(|cwd| crate::overlay::discover_overlay(cwd))
        .map(|path| crate::overlay::analyze_overlay(&path));
    let config = crate::overlay::load_config(overlay.as_ref()).context("loading config")?;
    let soul = crate::overlay::load_soul(overlay.as_ref()).context("loading soul")?;
    let memory = crate::overlay::load_memories(overlay.as_ref()).context("loading memory")?;
    let kstore = crate::overlay::load_knowledge(overlay.as_ref()).context("loading knowledge store")?;
    let skills = crate::overlay::discover_skills(overlay.as_ref()).context("discovering skills")?;

    let resolved = config.resolve_default();
    let mut agent = AgentSession::new(
        config,
        soul,
        memory,
        kstore,
        skills,
        resolved,
        SessionOptions::default(), // streaming + tools enabled, no system override
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
    let mcp_servers: Vec<String> = agent
        .mcp
        .server_names()
        .into_iter()
        .map(String::from)
        .collect();

    eprintln!("Daemon ready on {}", sock_path.display());
    eprintln!("  Model: {}", model);
    eprintln!("  MCP servers: {:?}", mcp_servers);

    // Set up signal handlers for graceful shutdown.
    // In daemon mode we use tokio's async signal handling rather than the ctrlc crate,
    // because (a) we need SIGTERM in addition to SIGINT, (b) async signal handling
    // properly wakes up the tokio select! loop, and (c) we want to ignore SIGHUP
    // since we're a daemon that has already detached from the terminal.
    //
    // We use a `Notify` to wake up the main accept loop immediately when a signal
    // arrives, rather than waiting for the idle-check timeout.
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_notify = Arc::new(tokio::sync::Notify::new());

    // SIGINT (Ctrl-C) — graceful shutdown
    let shutdown_int = shutdown.clone();
    let notify_int = shutdown_notify.clone();
    let _int_handle = tokio::spawn(async move {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
            Ok(mut sig) => {
                sig.recv().await;
                eprintln!("\nReceived SIGINT, shutting down...");
                shutdown_int.store(true, Ordering::SeqCst);
                notify_int.notify_one();
            }
            Err(e) => {
                eprintln!("Warning: could not install SIGINT handler: {}", e);
            }
        }
    });

    // SIGTERM — standard daemon shutdown (kill, systemctl stop, etc.)
    let shutdown_term = shutdown.clone();
    let notify_term = shutdown_notify.clone();
    let _term_handle = tokio::spawn(async move {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
                eprintln!("\nReceived SIGTERM, shutting down...");
                shutdown_term.store(true, Ordering::SeqCst);
                notify_term.notify_one();
            }
            Err(e) => {
                eprintln!("Warning: could not install SIGTERM handler: {}", e);
            }
        }
    });

    // SIGHUP — ignore (we're a daemon, terminal disconnect is expected)
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }

    // Listen on Unix socket
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding socket {}", sock_path.display()))?;

    let idle_timeout =
        Duration::from_secs(idle_timeout_mins.unwrap_or(DEFAULT_IDLE_TIMEOUT_MINS) * 60);
    let state = DaemonState {
        started_at: Instant::now(),
        idle_timeout,
    };
    let model_name = agent.resolved.model.clone();
    let base_url = agent.resolved.base_url.clone();

    // Main accept loop
    let mut last_activity = Instant::now();

    loop {
        // Check shutdown signal (SIGINT or SIGTERM via tokio signal handlers)
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        // Check idle timeout
        if last_activity.elapsed() > state.idle_timeout {
            eprintln!(
                "Idle timeout reached ({} min), shutting down.",
                state.idle_timeout.as_secs() / 60
            );
            break;
        }

        // Accept with timeout so we can check shutdown/idle periodically.
        // The shutdown_notify branch wakes us up immediately on signal receipt.
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        last_activity = Instant::now();
                        handle_connection(stream, &mut agent, &state, &model_name, &base_url, &shutdown, &shutdown_notify).await;
                    }
                    Err(e) => {
                        eprintln!("Error accepting connection: {}", e);
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(IDLE_CHECK_INTERVAL_SECS)) => {
                // Just loop around and check shutdown/idle
            }
            _ = shutdown_notify.notified() => {
                // Signal received — loop will check shutdown flag and break
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
        )
        .await
        {
            Ok(Ok(summary_text)) if !summary_text.is_empty() => {
                if let Err(e) = agent
                    .memory
                    .add_memory(format!("session_summary\n{}", summary_text))
                {
                    eprintln!("Warning: Failed to save summary: {}", e);
                }
            }
            _ => {
                let fallback = crate::summary::fallback_summary(&agent.messages);
                let _ = agent
                    .memory
                    .add_memory(format!("session_summary\n{}", fallback));
            }
        }
    }

    // Save knowledge store on exit
    if let Err(e) = agent.kstore.save() {
        eprintln!("Warning: Failed to save knowledge store: {}", e);
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
    shutdown: &AtomicBool,
    shutdown_notify: &tokio::sync::Notify,
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
        Request::Chat {
            prompt,
            model,
            system: _system,
            no_stream,
            no_tools,
        } => {
            // Switch model if requested
            if let Some(new_model) = model
                && let Err(e) = agent.switch_model(&new_model)
            {
                let err_event = Event::Error {
                    message: format!("Failed to switch model: {}", e),
                };
                if let Ok(jsonl) = err_event.to_jsonl() {
                    let _ = writer.write_all(format!("{}\n", jsonl).as_bytes()).await;
                }
                return;
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
                        if let Ok(jsonl) = event.to_jsonl()
                            && writer
                                .write_all(format!("{}\n", jsonl).as_bytes())
                                .await
                                .is_err()
                        {
                            break; // Client disconnected
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
            let event = Event::Done;
            if let Ok(jsonl) = event.to_jsonl() {
                let _ = writer.write_all(format!("{}\n", jsonl).as_bytes()).await;
            }
            // Set the shutdown flag and notify the main loop so it exits cleanly
            // (runs cleanup, MCP shutdown, session summary, etc.)
            eprintln!("Shutdown requested via socket.");
            shutdown.store(true, Ordering::SeqCst);
            shutdown_notify.notify_one();
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
    stream
        .write_all(format!("{}\n", jsonl).as_bytes())
        .await
        .context("sending request to daemon")?;
    stream
        .shutdown()
        .await
        .context("shutting down write side of daemon connection")?;

    let mut events = Vec::new();
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();
    while let Some(line) = lines
        .next_line()
        .await
        .context("reading response from daemon")?
    {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match Event::from_jsonl(&line) {
            Ok(event) => events.push(event),
            Err(e) => {
                eprintln!(
                    "Warning: failed to parse event from daemon: {} (line: {})",
                    e, line
                );
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
            if let Ok(jsonl) = ping.to_jsonl()
                && stream
                    .write_all(format!("{}\n", jsonl).as_bytes())
                    .await
                    .is_ok()
                && stream.shutdown().await.is_ok()
            {
                // Read response
                let reader = BufReader::new(stream);
                let mut lines = reader.lines();
                if let Ok(Some(line)) = lines.next_line().await
                    && let Ok(Event::Pong) = Event::from_jsonl(line.trim())
                {
                    return true;
                }
            }
            false
        }
        Err(_) => false,
    }
}

/// Start the daemon as a background process. Returns the daemon PID.
///
/// Uses the classic Unix double-fork pattern to properly detach from the
/// terminal: fork → setsid → fork → exec. This ensures the daemon:
/// - Is not a process group leader (can't reacquire a controlling terminal)
/// - Is in its own session (immune to SIGHUP from terminal exit)
/// - Has no controlling terminal
pub fn spawn_daemon(idle_timeout_mins: Option<u64>) -> Result<u32> {
    let exe = std::env::current_exe().context("finding current executable")?;

    // Clean up stale socket/pid files
    let sock = socket_path();
    let pid = pid_path();
    if sock.exists() {
        std::fs::remove_file(&sock).ok();
    }
    if pid.exists() {
        std::fs::remove_file(&pid).ok();
    }

    // ── Prepare everything BEFORE forking (async-signal-safe constraints) ──

    // Build the argument list ahead of time
    let mut args: Vec<String> = Vec::new();
    if let Some(mins) = idle_timeout_mins {
        args.push(format!("--idle-timeout={}", mins));
    }
    args.push("daemon".to_string());
    args.push("start".to_string());

    // Convert to C strings before forking (allocation is not async-signal-safe)
    let exe_cstr = std::ffi::CString::new(exe.to_string_lossy().into_owned())
        .context("converting exe path to C string")?;
    let mut c_args: Vec<std::ffi::CString> = vec![exe_cstr.clone()];
    for arg in &args {
        c_args.push(
            std::ffi::CString::new(arg.as_str()).context("converting daemon arg to C string")?,
        );
    }
    let mut argv: Vec<*const libc::c_char> = c_args.iter().map(|s| s.as_ptr()).collect();
    argv.push(std::ptr::null());

    // Create a pipe so the grandchild can report its PID back to us
    let mut pipe_fds: [std::os::fd::RawFd; 2] = [0, 0];
    unsafe {
        if libc::pipe(pipe_fds.as_mut_ptr()) != 0 {
            return Err(anyhow::anyhow!(
                "failed to create pipe for daemon PID: errno {}",
                *libc::__errno_location()
            ));
        }
    }
    let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);

    // ── First fork ──
    let first_pid = unsafe { libc::fork() };
    if first_pid < 0 {
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return Err(anyhow::anyhow!("first fork failed"));
    }

    if first_pid > 0 {
        // ── Original caller (parent) ──
        // Wait for the grandchild's PID via pipe, then return.
        unsafe {
            libc::close(write_fd);
        }

        let mut pid_buf = [0u8; 4];
        let n = unsafe { libc::read(read_fd, pid_buf.as_mut_ptr() as *mut libc::c_void, 4) };
        unsafe {
            libc::close(read_fd);
        }

        let result = if n == 4 {
            let daemon_pid = u32::from_be_bytes(pid_buf);
            // Reap the intermediate child (first fork child)
            unsafe {
                libc::waitpid(first_pid, std::ptr::null_mut(), 0);
            }
            Ok(daemon_pid)
        } else {
            unsafe {
                libc::waitpid(first_pid, std::ptr::null_mut(), 0);
            }
            Err(anyhow::anyhow!(
                "daemon failed to start: grandchild did not report PID"
            ))
        };
        return result;
    }

    // ── Intermediate child (first fork) ──
    // Close the read end — we don't need it.
    unsafe {
        libc::close(read_fd);
    }

    // Start a new session to detach from the controlling terminal.
    unsafe {
        if libc::setsid() < 0 {
            libc::_exit(1);
        }
    }

    // ── Second fork ──
    let second_pid = unsafe { libc::fork() };
    if second_pid < 0 {
        unsafe {
            libc::_exit(2);
        }
    }

    if second_pid > 0 {
        // Intermediate child: exit immediately. The grandchild is now orphaned
        // (adopted by init/systemd) and fully detached from any terminal.
        unsafe {
            libc::_exit(0);
        }
    }

    // ── Grandchild (actual daemon process) ──
    // Redirect stdin/stdout/stderr to /dev/null
    unsafe {
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, 0); // stdin
            libc::dup2(devnull, 1); // stdout
            libc::dup2(devnull, 2); // stderr
            libc::close(devnull);
        }
        // Reset umask so the daemon can create files with any permissions
        libc::umask(0o022);
        // Change working directory to home to avoid holding arbitrary mount points
        // (don't chdir to / — we need to write PID/socket files relative to home)
    }

    // Write our PID to the pipe so the original caller can read it
    let my_pid = unsafe { libc::getpid() };
    let pid_bytes = (my_pid as u32).to_be_bytes();
    unsafe {
        libc::write(write_fd, pid_bytes.as_ptr() as *const libc::c_void, 4);
        libc::close(write_fd);
    }

    // Set the FOREGROUND flag so the daemon child runs run_daemon() directly
    // Safety: we're in the grandchild process about to exec; no other threads exist.
    unsafe {
        std::env::set_var("__ENCHANTER_DAEMON_FOREGROUND", "1");
    }

    // Exec the enchanter binary as the daemon
    unsafe {
        libc::execv(exe_cstr.as_ptr(), argv.as_ptr());
        // If execv returns, it failed — no way to report error, just exit
        libc::_exit(3);
    }
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
                    Event::StatusInfo {
                        model,
                        mcp_servers,
                        uptime_secs,
                    } => {
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
pub async fn chat_via_daemon(
    prompt: &str,
    model: Option<String>,
    system: Option<String>,
    no_stream: bool,
    no_tools: bool,
) -> Result<Option<String>> {
    let request = Request::Chat {
        prompt: prompt.to_string(),
        model,
        system,
        no_stream,
        no_tools,
    };

    let mut stream = connect().await?;
    let jsonl = request.to_jsonl()?;
    stream
        .write_all(format!("{}\n", jsonl).as_bytes())
        .await
        .context("sending chat request to daemon")?;
    stream
        .shutdown()
        .await
        .context("shutting down write side of daemon connection")?;

    let mut full_response = String::new();
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    while let Some(line) = lines
        .next_line()
        .await
        .context("reading response from daemon")?
    {
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
                    Event::ToolResult { content, .. }
                        // Optionally show tool output (truncated)
                        if !content.is_empty() => {
                            let preview: String = content.lines().take(3).collect::<Vec<_>>().join("\n");
                            let truncated = if content.lines().count() > 3 {
                                format!("{}\n  {}({} more lines)", preview.dimmed(), "↳".dimmed(), content.lines().count() - 3)
                            } else {
                                preview
                            };
                            println!("  {}", truncated);
                        }
                    Event::Compacted { removed_messages, budget_tokens } => {
                        eprintln!(
                            "\n  {} Compacted {} earlier message(s) to stay within the context budget (~{} tokens).",
                            "⟡".dimmed(), removed_messages, budget_tokens
                        );
                    }
                    Event::Done
                        if !full_response.is_empty() => {
                            println!(); // Trailing newline after content
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
