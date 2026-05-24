# Enchanter Daemon Mode — Implementation Plan

## Goal

Make enchanter respond to the first prompt instantly by keeping a warm daemon
process running in the background. When the user types `enchanter` or
`enchanter -p "question"`, the CLI connects to the daemon via Unix socket instead
of cold-starting from scratch.

## Problem

Current cold-start cost:
- Binary startup: ~5ms (Rust, negligible)
- Config/memory/skills load: ~10-50ms (disk I/O, acceptable)
- System prompt assembly: ~1ms (negligible)
- MCP server startup: **2-15 seconds** (spawning npx processes, the real bottleneck)
- LLM API call: 1-5 seconds (unavoidable, but overlapped)

The user perceives a 3-15 second delay before seeing any output. Most of the
pain is MCP servers. The daemon keeps them warm.

## Architecture

```
┌─────────────┐       Unix socket        ┌─────────────────┐
│  enchanter   │◄────────────────────────►│  enchanterd     │
│  (thin CLI)  │   ~/.enchanter/sock      │  (daemon)      │
└─────────────┘                           │                 │
                                          │  - config       │
                                          │  - soul         │
                                          │  - memory       │
                                          │  - skills       │
                                          │  - MCP conns    │
                                          │  - system prompt │
                                          └─────────────────┘
```

The CLI becomes a thin relay: parse args, connect to socket, send request,
stream response to terminal. The daemon does all the real work.

## Implementation Steps

### Phase 1: Conversation state persistence

Before daemon mode works, we need the ability to serialize and restore
conversations. This also enables session replay, branching, and crash recovery.

**1.1 Define session file format** (`~/.enchanter/sessions/<id>.jsonl`)

```jsonl
{"type":"system","content":"..."}
{"type":"user","content":"hello"}
{"type":"assistant","content":"hi there"}
{"type":"tool_call","id":"1","name":"read_file","arguments":"..."}
{"type":"tool_result","id":"1","content":"..."}
```

- One JSON object per line (JSONL — easy to append, easy to parse)
- Each line is a self-contained message
- Session ID is a UUID generated on REPL start
- File is appended to on every turn (never rewritten — crash-safe)

**Files:** `src/session.rs` (new), `src/cli/mod.rs` (write turn-by-turn)

**1.2 Save sessions on exit and on each turn**

- On REPL startup: generate session ID, create session file
- On each agent turn: append messages to the session file
- On clean exit: session file stays (no cleanup)
- On crash: partial session file is still valid (JSONL is crash-safe)

**1.3 Add `enchanter session list` and `enchanter session show <id>`**

- List recent sessions with timestamp and message count
- Show a session's full conversation (replay)

### Phase 2: Daemon infrastructure

**2.1 Define the socket protocol**

Request (client → daemon):
```json
{
  "type": "chat",
  "prompt": "explain ownership",
  "model": null,
  "system": null,
  "no_stream": false,
  "no_tools": false
}
```

Response (daemon → client): streaming JSONL
```jsonl
{"type":"content","text":"Ownership is..."}
{"type":"tool_call","id":"1","name":"exec_command","arguments":"..."}
{"type":"tool_result","id":"1","content":"..."}
{"type":"content","text":"So in summary..."}
{"type":"done"}
```

Control messages:
```json
{"type":"ping"}         → {"type":"pong"}
{"type":"status"}       → {"type":"status","model":"...","mcp_servers":[...]}
{"type":"shutdown"}     → daemon exits gracefully
```

**2.2 Implement `enchanter daemon start` and `enchanter daemon stop`**

- `enchanter daemon start` → fork to background, create PID file, listen on socket
- `enchanter daemon stop` → send shutdown message via socket
- Auto-start: if `enchanter` (or `enchanter -p ...`) can't connect to the socket,
  spawn the daemon and wait for it to be ready (with timeout)

The daemon initializes: load config, soul, memory, skills, start MCP servers.
Then listens on the socket for requests.

**Files:** `src/daemon.rs` (new), `src/cli/mod.rs` (refactor)

**2.3 Refactor `cli/mod.rs` into client + daemon paths**

Current `run_repl()` and `run_single()` hold all the session logic. Extract
into a `Session` struct that can be used by both the daemon and the CLI:

```rust
struct Session {
    config: Config,
    soul: Soul,
    memory: MemoryStore,
    skills: SkillsIndex,
    mcp: McpManager,
    messages: Vec<Message>,
    resolved: ResolvedModel,
    client: LlmClient,
}
```

The daemon holds one or more `Session` instances. The CLI sends requests to
the daemon and relays responses.

**2.4 Implement socket client in the CLI**

When the user runs `enchanter`:
1. Try to connect to `~/.enchanter/sock`
2. If connected: send request, stream response
3. If not connected: optionally auto-start daemon, then connect
4. Fallback: if daemon is unavailable and `--no-daemon` flag, run inline (current behavior)

### Phase 3: Daemon lifecycle

**3.1 Auto-start daemon on first use**

- If socket doesn't exist, CLI spawns daemon as a background process
- Wait for socket to appear (poll with timeout, default 30s for MCP startup)
- If timeout: fall back to inline mode with a warning

**3.2 Idle timeout**

- Daemon shuts down after N minutes of inactivity (default: 10, configurable)
- Prevents zombie processes eating resources
- Next `enchanter` invocation auto-starts again

**3.3 Daemon health check**

- CLI sends ping on connect
- If daemon is unresponsive: kill stale process, start fresh
- Stale socket files cleaned up on start

**3.4 Signal handling**

- SIGTERM: graceful shutdown (close MCP servers, flush memory, save session)
- SIGINT (Ctrl+C from daemon process): ignored (daemon is background)
- SIGHUP: reload config, SOUL, skills, restart MCP servers

### Phase 4: Polish

**4.1 `enchanter daemon status`**

Show: PID, uptime, model, MCP servers with status, memory entry count,
sessions served.

**4.2 Session continuity**

When CLI connects to daemon, it can request a new session or resume an
existing one (by session ID). This enables true conversation persistence
across terminal sessions.

**4.3 `--no-daemon` flag**

Force inline mode (current behavior) for users who don't want the daemon.
Document this as the escape hatch.

**4.4 MCP server restart on crash**

Daemon monitors MCP server processes. If one crashes, restart it (up to
MAX_RESTARTS as currently implemented). Already in `mcp/mod.rs` but needs
daemon-awareness (log to daemon log instead of stderr).

## Scope boundaries — NOT doing

- System-level service (systemd/runit) — that's option B, future work
- Multi-user server — enchanter is single-user, single-seat
- TUI / rich terminal UI — stay faithful to simple streaming output
- RAG / vector store — discussed and deferred
- Plugin marketplace — out of scope

## New files

- `src/session.rs` — conversation state, JSONL persistence, session management
- `src/daemon.rs` — daemon process, socket listener, lifecycle

## Modified files

- `src/main.rs` — add daemon subcommand routing
- `src/cli/mod.rs` — refactor into thin client + Session struct
- `src/mcp/mod.rs` — daemon-aware logging, restart tracking
- `Cargo.toml` — add `tokio::net::UnixListener` (already have tokio), add `uuid`

## Dependency changes

- `uuid` — for session IDs
- `serde_json` — already present, used for socket protocol
- No new system dependencies — Unix sockets are in `tokio::net`

## Testing strategy

- Unit tests for session serialization/deserialization
- Integration test: start daemon, send chat request, verify response
- Integration test: daemon auto-start when socket doesn't exist
- Integration test: daemon idle timeout
- Integration test: graceful shutdown with active MCP connections
- `--no-daemon` mode must pass all existing tests unchanged

## Risk assessment

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Socket permission issues on multi-user systems | Low | Socket in ~/.enchanter with 0600 permissions |
| Daemon hang/crash leaving stale socket | Medium | Health check on connect, auto-cleanup stale PID/socket |
| MCP server startup still slow even with daemon | Medium | Show "warming up..." message during initial startup |
| Session state grows unbounded in memory | Low | Cap conversation history, persist to disk after each turn |
| Complex refactoring of cli/mod.rs | Medium | Phase 2.3 first — extract Session struct before touching daemon code |