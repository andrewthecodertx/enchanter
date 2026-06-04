# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Enchanter is a single-binary Rust AI agent harness that talks to any OpenAI-compatible
provider (OpenAI, OpenRouter, Ollama, etc.). It has zero runtime dependencies: it reads a
persona (SOUL.md), loads memory, discovers skills, and runs an agentic tool-calling loop.

## Build, test, run

```bash
cargo build                      # debug build
cargo build --release            # release (LTO + strip; what `make install` ships)
make install                     # release build → ~/.local/bin/enchanter (PREFIX overridable)

cargo test                       # run all tests (unit tests live inline in each module)
cargo test <name>                # run a single test by substring, e.g. `cargo test prompt_contains_soul`
cargo test <module>::            # run a module's tests, e.g. `cargo test config::`

cargo clippy --all-targets       # lint
cargo fmt                        # format

cargo build --no-default-features   # build without the TUI feature
```

Edition is **2024** (requires a recent stable toolchain). Tests are colocated with the code in
`#[cfg(test)]` modules — there is no `tests/` directory.

## Runtime data directory

Enchanter operates on `~/.enchanter/` (override with `ENCHANTER_HOME`), created on first run by
`home::init_home()`. It contains `SOUL.md`, `config.yaml`, `memories/`, `skills/`, and
`sessions/`. Scaffold templates live in `src/scaffold/`. When testing behavior that touches the
home dir, set `ENCHANTER_HOME` to a temp path rather than mutating your real one.

## Architecture (the big picture)

The flow is **config → state load → AgentSession → agent loop**, with three frontends (REPL,
single-shot `-p`, TUI) and an optional daemon, all sharing the same core.

**`AgentSession` (`src/agent.rs`) is the heart.** It owns all conversation state (config, soul,
memory, skills, MCP manager, message history, resolved model, LLM client, session log) and the
agent loop. `run_loop` calls the model, dispatches any tool calls, appends results, and repeats
until the model returns plain text or `max_turns` is hit. A `soft_limit` injects a "wrap up now"
nudge a few turns before the hard limit. Both REPL and daemon drive this same loop — display
concerns are decoupled via an `EventSink` (either a `Silent` sink or an `mpsc::Channel` emitting
`protocol::Event`s for streaming over a socket).

**Tool dispatch is two-tier (`agent.rs::dispatch_tool`).** Built-in tools (`exec_command`,
`read_file`, `write_file`, `edit_file`, `search_files`, `list_directory`, `memory`) are handled
in `src/tools/mod.rs`. Anything whose name contains `:` is routed to MCP (`src/mcp/mod.rs`).
The combined tool schema sent to the model is `tools::tools_json()` + `mcp.all_tools_json()`.

**Filesystem sandbox (two layers).** Built-in *file* tools (`read_file`/`write_file`/`edit_file`/
`search_files`/`list_directory`) validate the target path in-process via `resolve_and_validate` /
`path_is_allowed` against `security.allowed_paths` (defaults to `$HOME`; `SecurityConfig::resolve`).
That check can't contain a *shell*, so `exec_command` is additionally confined by the OS: on Linux
it runs under **Landlock** (`src/sandbox.rs`), restricting the shell and its children to read/write
within `allowed_paths` (+ `/tmp`, curated `/dev`) and read/execute on system dirs. Because Landlock
must be applied single-threaded before exec (the agent is multithreaded tokio), `exec_command`
re-execs the binary as a hidden `__sandboxed-exec` helper — `main()` intercepts that arg *before*
starting the runtime, applies Landlock, then `exec`s `sh -c`. If no sandbox is available (old
kernel / non-Linux), `exec_command` fails closed unless `security.allow_unsandboxed_exec: true`.
When adding a file-touching tool, keep the `path_is_allowed` check; when changing exec, preserve
the single-threaded re-exec path. End-to-end coverage is in `tests/sandbox.rs` (Linux only).

**Layered system prompt (`src/prompt/mod.rs`).** The prompt is assembled in ordered layers:
SOUL → CONTEXT (environment) → SKILLS → INSTRUCTIONS → VOLATILE (memory) → SESSION (timestamp).
All assembly flows through `build_prompt_layers`, which returns a `PromptLayers` struct that both
the live prompt and the inspection commands (`/prompt`, `/prompt diff`, `/prompt budget` in
`src/prompt/inspect.rs`) consume — so what you inspect is exactly what the model receives. If you
change prompt content, change it here, not in a separate code path.

**Config layering (global truth + project overlay).** `Config` (`src/config/mod.rs`) loads from
`~/.enchanter/config.yaml`. Resolution precedence for model/url/key is config → `ENCHANTER_*`
env → `OPENAI_*` env → built-in default. A project `.enchanter/` directory (discovered by walking
up from cwd to the git root, `src/overlay.rs`) layers **additively**: global is always
authoritative, the project overlay only *adds* MCP servers/providers/skills/memories and *appends*
a project SOUL — it never overrides global values. Named providers in config inherit missing
fields from the top-level model config; `/model <name>` switches the whole provider preset.

**MCP (`src/mcp/mod.rs`).** Supports stdio (spawned child processes, auto-restarted up to 3×) and
Streamable HTTP (handles both JSON and SSE responses, tracks `Mcp-Session-Id`). `${VAR}` in
config values is expanded via shellexpand. Tools are namespaced `server:tool`.

**Daemon (`src/daemon.rs`, Unix only).** A background process keeps MCP servers warm to avoid
cold-start latency. It listens on `~/.enchanter/sock`, streams `Event`s as JSONL over the socket,
and auto-shuts down after idle timeout. Single-shot `enchanter -p` auto-starts it and falls back
to inline mode if unreachable. On Windows the whole module is a `bail!`-ing stub (no Unix sockets)
— guard any daemon code with `#[cfg(unix)]` and keep the stub signatures in sync.

**Sessions & recording.** Every conversation is appended turn-by-turn to a crash-safe JSONL file
in `sessions/` (`src/session.rs`). `--record` writes a richer replayable recording
(`src/recorder.rs`) with config snapshots (API keys redacted), prompt-layer hashes, and tool
calls; `enchanter replay` (`src/replay.rs`) re-runs it, optionally swapping the model or stubbing
tools for deterministic playback.

**TUI (`src/tui/`, behind the default `tui` feature).** A ratatui/crossterm multi-pane interface
that drives the same `AgentSession`. Gate TUI-only code with `#[cfg(feature = "tui")]`.

## Conventions worth knowing

- Built-in tool names follow Claude Code's naming (Bash→`exec_command`, Read→`read_file`, etc.);
  module-level doc comments in each file cite the upstream projects (hermes-agent, OpenCode,
  Claude Code) that informed each subsystem — useful context when extending one.
- Async throughout (`tokio` full); the agent loop and all I/O are async.
- `Event` (`src/protocol.rs`) is the wire/display protocol shared by streaming, daemon, and TUI.
  Adding a new kind of agent output generally means adding an `Event` variant and handling it in
  each sink/frontend.
