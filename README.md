# Enchanter

A focused AI agent harness. Single Rust binary, any OpenAI-compatible provider, zero runtime dependencies.

Reads your SOUL, loads your memory, finds your skills, talks to your model. Nothing more.

**[Full documentation →](https://andrewthecoder.com/projects/enchanter)**

## Setup

First run takes care of itself. Just launch it:

```bash
enchanter
```

It creates `~/.enchanter/` with everything you need:

- `SOUL.md` — your agent's personality
- `config.yaml` — model, provider, API key
- `memories/` — persistent memory files
- `knowledge/` — structured key-value facts (kstore.json)
- `skills/` — drop in SKILL.md files
- `sessions/` — conversation history (JSONL)

Then configure a provider in `config.yaml`. A few examples:

```yaml
# Ollama (local, no API key needed)
model:
  default: qwen3
  base_url: http://localhost:11434/v1/chat/completions

# OpenAI
model:
  default: gpt-4.1-mini
  api_key: sk-...

# OpenRouter
model:
  default: anthropic/claude-sonnet-4
  base_url: https://openrouter.ai/api/v1/chat/completions
  api_key: sk-or-...
```

You can also use environment variables instead of the config file:

```bash
export ENCHANTER_MODEL=gpt-4.1-mini
export ENCHANTER_API_KEY=sk-...
export ENCHANTER_BASE_URL=https://api.openai.com/v1/chat/completions
```

Or point the whole data directory elsewhere:

```bash
export ENCHANTER_HOME=/path/to/your/config
```

### Named providers

Define multiple providers in `config.yaml` and switch between them mid-session:

```yaml
providers:
  ollama:
    model: qwen3
    base_url: http://localhost:11434/v1/chat/completions
  openrouter:
    model: anthropic/claude-sonnet-4
    base_url: https://openrouter.ai/api/v1/chat/completions
    api_key: sk-or-...
```

Then in the REPL:

```
/model ollama       # Switches model, base_url, and api_key
/model openrouter   # Full provider switch
/model gpt-4.1      # Bare model ID keeps current provider
```

## Install

From source (Linux/macOS):

```bash
git clone https://github.com/andrewthecodertx/enchanter.git
cd enchanter
make install
```

This builds a release binary and installs it to `~/.local/bin/enchanter`.

From source (Windows):

```bash
git clone https://github.com/andrewthecodertx/enchanter.git
cd enchanter
cargo build --release
```

The binary will be at `target\release\enchanter.exe`. Copy it anywhere on your `PATH`.

## Running

```bash
# Interactive session
enchanter

# Ask one question and exit
enchanter -p "Explain Rust ownership in one paragraph"

# Use a different model for this session
enchanter -m qwen3

# Override the system prompt
enchanter -s "You are a pirate. Always respond in pirate speech."

# Disable streaming (wait for the full reply)
enchanter --no-stream -p "Summarize this"

# Disable all tools (built-in + MCP)
enchanter --no-tools -p "What is 2+2?"

# Record the session to a JSONL file
enchanter --record session.jsonl

# Record with additional field redaction
enchanter --record session.jsonl --record-redact
```

### TUI mode

```bash
# Launch the terminal UI (multi-pane, keyboard-driven)
enchanter tui
```

The TUI is a multi-pane interface with a sidebar (skills + memory), main chat area, and input bar. A header shows the model, provider, and session info; a footer shows tool/skill counts and quick key hints.

**Focus and navigation:**

| Key             | Action                                |
|-----------------|---------------------------------------|
| `Tab`           | Cycle focus forward through panes     |
| `Shift+Tab`     | Cycle focus backward                  |
| `1`–`4`         | Jump to Skills / Memory / Chat / Input|
| `/`             | Jump to input pane and start a command|
| `Esc`           | Return focus to input pane            |

**Input bar:**

| Key                   | Action                                |
|-----------------------|---------------------------------------|
| `Enter`               | Send message (multiline off) / newline (multiline on) |
| `Ctrl+Enter`          | Newline (multiline off) / send (multiline on) |
| `Ctrl+M`              | Toggle multiline mode                 |
| `Ctrl+A` / `Home`     | Move cursor to start                  |
| `Ctrl+E` / `End`      | Move cursor to end                    |
| `Ctrl+U`              | Clear input line                      |
| `←`/`→`              | Move cursor                           |
| `Backspace` / `Delete` | Delete character                      |

**Sidebar panes (Skills / Memory):**

| Key              | Action                                |
|------------------|---------------------------------------|
| `↑`/`↓` or `j/k` | Navigate items                         |
| `Enter`          | Show details in chat                   |
| `?`              | Show help in chat                      |

**Chat pane:**

| Key              | Action                                |
|------------------|---------------------------------------|
| `↑`/`↓` or `j/k` | Scroll up/down one line               |
| `PageUp`/`PageDown` | Scroll by page                       |
| `End`            | Jump to bottom / re-enable auto-scroll|
| `?`              | Show help in chat                      |

**During streaming:**

| Key             | Action                                |
|-----------------|---------------------------------------|
| `Ctrl+C`        | Cancel streaming response             |
| `Ctrl+Q`        | Quit                                   |
| `Tab`/`Shift+Tab` | Cycle focus (even while streaming)   |

All REPL slash commands work in the TUI (`/help`, `/clear`, `/model`, `/undo`, `/retry`, etc.). On exit, the TUI generates a session summary to memory just like the REPL.

The TUI is an optional feature (enabled by default). Build without it: `cargo build --no-default-features`.

### Inside the REPL

| Command            | What it does                                       |
|--------------------|-----------------------------------------------------|
| `/help`            | Show available commands                             |
| `/clear`           | Reset conversation history                           |
| `/ctx`             | Show context token usage and budget                   |
| `/soul`            | Show SOUL.md content                                |
| `/memory`          | Show loaded memory                                   |
| `/skills`          | List discovered skills                               |
| `/tools`           | List all available tools                             |
| `/model <name>`    | Switch model or named provider                       |
| `/prompt`          | Show full system prompt                              |
| `/prompt diff`     | Show diff of system prompt from previous turn         |
| `/prompt budget`   | Show token/character budget per prompt layer           |
| `/retry`           | Re-send the last user message                         |
| `/undo`            | Remove last exchange from history                     |
| `/config`           | Show resolved configuration (includes context usage)  |
| `/sessions`        | List session history                                  |
| `/exit`, `/quit`, `/bye` | Quit (also Ctrl+D for clean exit)             |

## Prompt inspection

Enchanter builds the system prompt in layers (SOUL → CONTEXT → SKILLS → INSTRUCTIONS → KNOWLEDGE → VOLATILE → SESSION). You can inspect exactly what the model receives:

```bash
# Show the full assembled system prompt
enchanter prompt

# Show a token/character budget breakdown per layer
enchanter prompt --budget

# Show the diff between the previous and current turn's prompt
# (available in REPL as /prompt diff)
```

The budget view shows approximate token counts per layer (using a chars÷4 heuristic), visual bar charts, and threshold warnings when a layer exceeds ~4,000 estimated tokens.

## Recording and replay

Record a full session to JSONL for debugging, reproducibility, or model comparison:

```bash
# Record a session
enchanter --record session.jsonl -p "Explain Rust ownership"

# Replay a recorded session
enchanter replay session.jsonl

# Replay with a different model
enchanter replay session.jsonl --swap-model gpt-4

# Replay in exact mode (error if model doesn't match)
enchanter replay session.jsonl --exact

# Replay with stubbed tools (deterministic, no live tool calls)
enchanter replay session.jsonl --tools stubbed
```

Recordings include schema version, monotonic sequence numbers, timestamps, config snapshots (with API keys redacted), prompt layer hashes, messages, tool calls, and model changes. API keys and auth tokens are never included in recordings by default.

## Platform support

| Platform | REPL & inline | Daemon mode |
|----------|:---:|:---:|
| Linux    | ✅ | ✅ |
| macOS    | ✅ | ✅ |
| Windows  | ✅ | ❌ |

Daemon mode requires Unix domain sockets, which aren't available on Windows. On Windows, Enchanter always runs in inline mode — no `--no-daemon` flag or `daemon` subcommand is needed or available.

## Daemon mode

> **Unix only** — daemon mode requires Unix domain sockets and is available on Linux and macOS. On Windows, Enchanter always runs in inline mode.

Enchanter can run as a background daemon that keeps MCP servers warm. This
eliminates the 3–15 second cold start on every invocation (most of which is
spawning MCP server processes).

```bash
# Start the daemon
enchanter daemon start

# Check status
enchanter daemon status

# Stop
enchanter daemon stop
```

The daemon listens on `~/.enchanter/sock` and writes its PID to
`~/.enchanter/daemon.pid`. It auto-shuts down after 10 minutes of inactivity
(configurable with `--idle-timeout`).

**Auto-start:** when you run `enchanter -p "question"` and the daemon isn't
running, Enchanter will start it automatically, wait for it to become ready,
then send your request through it. This gets you fast responses with zero
setup.

**Fallback:** if the daemon can't be reached, Enchanter falls back to inline
mode (current behavior). Use `--no-daemon` to skip the daemon entirely:

```bash
enchanter --no-daemon -p "quick question"
```

The daemon streams responses as JSONL events over the Unix socket, so you
still see content tokens as they arrive — not just a final blob of text.

## Info subcommands

```bash
enchanter soul          # Show current SOUL.md
enchanter memory        # Show loaded memory
enchanter skills        # List discovered skills
enchanter config        # Show resolved configuration
enchanter prompt        # Show assembled system prompt
enchanter prompt --budget  # Show token/character budget per layer
enchanter prompt --diff     # Show prompt layer structure
enchanter sessions      # List session history
enchanter sessions <id> # Show a specific session
enchanter replay <file.jsonl>  # Replay a recorded session
enchanter daemon status # Show daemon status (model, MCP, uptime)
enchanter init          # Scaffold .enchanter/ overlay in current directory
```

### Session summaries

When you exit the REPL with `/exit`, `/quit`, `/bye`, or Ctrl+D, Enchanter generates a concise summary of your session and saves it to memory. Your next session automatically loads this context, so you can pick up where you left off.

- Summaries are skipped for single-shot mode (`-p` flag)
- Skipped if the session was too short (no real exchange)
- Timeout of 10 seconds; falls back to a simple message count on failure
- Disable with `summarize_on_exit: false` in the `agent` section of config.yaml

> **Ctrl+C is a force-quit.** It bypasses the exit hook, so no session summary is saved. Use `/exit`, `/quit`, `/bye`, or Ctrl+D for a clean exit.

### Session history

Every conversation is automatically saved to `~/.enchanter/sessions/` as a JSONL file. Each session gets a unique ID, and every message (user, assistant, tool calls, tool results) is recorded turn-by-turn. The format is crash-safe — if the process dies mid-session, the file contains everything written up to that point.

List recent sessions:

```bash
enchanter sessions
```

View a specific session:

```bash
enchanter sessions <id>
```

Inside the REPL, use `/sessions` to list session history.

Sessions are also used internally for crash recovery and will power upcoming features like session branching and replay.

## Knowledge store

Unlike memory (free-form narrative text), the knowledge store captures discrete, typed facts that persist across sessions. Keys use dot-namespaced identifiers (e.g., `project.rust_version`, `user.email`) and values are short strings. Categories group related facts for search.

Each entry tracks its source:

- **observed** — detected from tool output, filesystem, or runtime
- **told** — explicitly stated by the user
- **inferred** — concluded by the agent from context

The agent proactively stores facts it discovers during conversation using the `knowledge` tool. When it learns a version number, a project path, a user preference, or a bug workaround, it stores it so it never has to ask again. The INSTRUCTIONS prompt layer directs this behavior; no separate classifier is needed.

Five categories are supported:

- **environment** — runtime and system facts
- **project** — project-specific details
- **preference** — user preferences and style
- **decision** — architectural or design decisions
- **fact** — general facts that don't fit other categories

The store lives at `~/.enchanter/knowledge/kstore.json` and is human-readable, git-friendly, and portable. It persists to disk on every write (store, forget) so crashes don't lose data.

### Project-level knowledge

When you run enchanter inside a project with a `.enchanter/` directory, project-level knowledge is merged on top of the global store. Project entries override global entries with the same key.

Place a `kstore.json` in your project's `.enchanter/knowledge/` directory:

```
my-project/
  .enchanter/
    knowledge/
      kstore.json      # project-specific facts
```

This lets you store project-specific facts (framework version, project paths, team conventions) alongside global facts, with project values taking precedence. Running `enchanter init` in a project directory creates the `knowledge/` directory automatically.

## How it works

The system prompt is built in layers:

1. **SOUL** — your persona from SOUL.md, stable across turns
2. **CONTEXT** — environment info (model, user, cwd, host, platform)
3. **SKILLS** — discovered SKILL.md files index
4. **INSTRUCTIONS** — tool usage guidance, canonical tool descriptions, and knowledge capture directives
5. **KNOWLEDGE** — structured key-value facts from the knowledge store
6. **VOLATILE** — memory entries, user profile
7. **SESSION** — timestamp

Each layer can be inspected via `/prompt budget` and compared across turns with `/prompt diff`.

Memory uses the same `§`-delimited format as Hermes Agent. Skills use the
same SKILL.md format (agentskills.io). If you're coming from Hermes, just
copy or symlink your data — the structure matches.

Sessions are saved as JSONL files in `~/.enchanter/sessions/`. Each conversation
turn is appended atomically, so the file is safe against crashes.

## MCP servers

Enchanter supports two MCP transport types:

- **stdio** — local processes spawned by Enchanter
- **HTTP** — remote servers reached via POST requests

Configure them in `~/.enchanter/config.yaml`:

```yaml
mcp:
  servers:
    filesystem:                                   # stdio transport
      command: npx
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/home/user"]
    fetch:                                        # stdio transport
      command: uvx
      args: ["mcp-server-fetch"]
    my-remote:                                    # http transport
      url: https://mcp.example.com/api
      headers:
        Authorization: "Bearer ${MY_TOKEN}"
```

Stdio servers are auto-restarted on crash (up to 3 attempts). HTTP servers
use the Streamable HTTP transport — they handle both direct JSON responses
and SSE-streamed responses, with `Mcp-Session-Id` tracking.

## Security sandbox

On Linux, Enchanter confines `exec_command` (shell execution) using Landlock,
a kernel-level LSM that restricts file access to an allowlist. The sandboxed
shell can only read/write within `allowed_paths` and read/execute system
directories (`/usr`, `/bin`, `/lib`, etc.). Write access to system dirs is
denied.

Configure it in `config.yaml`:

```yaml
security:
  # Directories the agent may read/write. Defaults to your home directory.
  allowed_paths:
    - ~/Projects
    - /tmp
  # On Linux, exec_command runs under Landlock by default. If the kernel
  # can't enforce it (old kernel, non-Linux), exec_command refuses to run.
  # Set this true as an escape hatch (use with care).
  allow_unsandboxed_exec: false
```

On **macOS and Windows**, there is no Landlock support. If
`allow_unsandboxed_exec` is `false` (the default), `exec_command` will refuse
to run. Set it to `true` to allow unsandboxed shell execution — understand
that this means the LLM can run any command your user can.

File tools (`read_file`, `write_file`, `edit_file`, `search_files`,
`list_directory`) always check `allowed_paths` regardless of platform.

## Agent configuration

The `agent` section of `config.yaml` controls the agent loop and conversation
behavior:

```yaml
agent:
  max_turns: 150           # Max agent loop turns (0 = unlimited, default: 150)
  summarize_on_exit: true  # Generate session summary on clean exit (default: true)
  memory:
    max_entries: 50         # Max memory entries loaded into prompt (default: 50)
    summarize_threshold: 40 # Summarize older entries when count exceeds this (default: 40)
  context:                            # Rolling conversation context (long sessions)
    max_tokens: 96000      # Compact older turns when estimated tokens exceed this
    keep_last_turns: 20    # Always keep this many recent messages verbatim
```

`max_turns` sets a hard limit on how many agent loop iterations (model calls)
can occur in a single session. When the limit is approached, a soft-limit
nudge reminds the agent to wrap up. Set to `0` for unlimited. Default is 150.

The `context` block controls rolling compaction for long sessions. When the
estimated token count exceeds `max_tokens` (estimated at chars÷4), older
turns are summarized into a single compact message. The most recent
`keep_last_turns` messages are always preserved verbatim. Both values are
optional — without them, no compaction occurs and the full conversation is
always sent.

The status bar (REPL and TUI) shows real-time context usage:
`ctx:35% 28k/128k` — the percentage and estimated token count relative to
the model's context window. Use `/ctx` to see the same information in the
chat. Models with known context sizes show the budget; unknown models show
only the estimated count.

## Provider compatibility

Enchanter sends tools (function calling) using the OpenAI `tools` format.
Most OpenAI-compatible providers support this, but not all.

### Tool calling support

| Provider | Tools work? | Notes |
|----------|:-----------:|-------|
| OpenAI | ✅ | Full support on all current models |
| OpenRouter | ✅ | Passes tools through to the underlying model — support depends on the model |
| Ollama | ✅ | Support depends on the model (e.g., qwen3, llama3.3, mistral work) |
| Groq | ✅ | Support depends on the model |
| DeepSeek | ✅ | Full support |
| Mistral | ✅ | Full support |
| LM Studio | ✅ | Proxy for local models; support depends on the model |
| **Perplexity** | ❌ | No tool calling support on any model or endpoint |

### No-tool mode

If your provider doesn't support tools, Enchanter still works — it just can't
use built-in tools or MCP servers. You have two options:

1. **Disable tools entirely** — useful for pure Q&A or search-augmented models:
   ```bash
   enchanter --no-tools -p "What causes tides?"
   ```
2. **Use a different provider** — switch to one that supports tools when you need
   file access, shell execution, or MCP integrations.

### Perplexity specifics

Perplexity has two endpoints, neither of which supports OpenAI-style tool calling:

- **`/chat/completions`** — their Sonar/search endpoint. Returns web-grounded
  answers with citations. Rejects the `tools` parameter with
  *"Tool calling is not supported for this model"*.
- **`/v1/agent`** — a different API format that is **not** OpenAI-compatible.
  It expects a different request schema and rejects standard OpenAI tool
  definitions.

Perplexity works fine for web-augmented Q&A with `--no-tools`. For agent-style
workflows with tools, use a different provider.

## License

MIT, see [LICENSE](LICENSE).

## Contributing

PRs welcome. Please open an issue first for major changes.
