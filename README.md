# Enchanter

A focused AI agent harness. Single Rust binary, any OpenAI-compatible provider, zero runtime dependencies.

Reads your SOUL, loads your memory, finds your skills, talks to your model. Nothing more.

## Setup

First run takes care of itself. Just launch it:

```bash
enchanter
```

It creates `~/.enchanter/` with everything you need:

- `SOUL.md` — your agent's personality
- `config.yaml` — model, provider, API key
- `memories/` — persistent memory files
- `skills/` — drop in SKILL.md files

Then configure a provider in `config.yaml`. A few examples:

```yaml
# Ollama (local, no API key needed)
model:
  default: qwen3
  base_url: http://localhost:11434/v1

# OpenAI
model:
  default: gpt-4.1-mini
  api_key: sk-...

# OpenRouter
model:
  default: anthropic/claude-sonnet-4
  base_url: https://openrouter.ai/api/v1
  api_key: sk-or-...
```

You can also use environment variables instead of the config file:

```bash
export ENCHANTER_MODEL=gpt-4.1-mini
export ENCHANTER_API_KEY=sk-...
export ENCHANTER_BASE_URL=https://api.openai.com/v1
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
    base_url: http://localhost:11434/v1
  openrouter:
    model: anthropic/claude-sonnet-4
    base_url: https://openrouter.ai/api/v1
    api_key: sk-or-...
```

Then in the REPL:

```
/model ollama       # Switches model, base_url, and api_key
/model openrouter   # Full provider switch
/model gpt-4.1      # Bare model ID keeps current provider
```

## Install

From source:

```bash
git clone https://github.com/andrewthecoder/enchanter.git
cd enchanter
make install
```

This builds a release binary and installs it to `~/.local/bin/enchanter`.

## Running

```bash
# Interactive session
enchanter

# Ask one question and exit
enchanter -p "Explain Rust ownership in one paragraph"

# Use a different model for this session
enchanter -m qwen3

# Disable streaming (wait for the full reply)
enchanter --no-stream -p "Summarize this"
```

### Inside the REPL

| Command    | What it does                            |
|------------|------------------------------------------|
| `/help`    | Show available commands                  |
| `/clear`   | Reset conversation history                |
| `/soul`    | Show SOUL.md content                     |
| `/memory`  | Show loaded memory                        |
| `/skills`  | List discovered skills                    |
| `/tools`   | List all available tools                  |
| `/model`   | Switch model or named provider            |
| `/retry`   | Re-send the last user message             |
| `/undo`    | Remove last exchange from history         |
| `/config`  | Show resolved configuration               |
| `/prompt`  | Show full system prompt                   |
| `/exit`    | Quit (also Ctrl+D for clean exit)        |

### Info subcommands

```bash
enchanter soul      # Show current SOUL.md
enchanter memory    # Show loaded memory
enchanter skills    # List discovered skills
enchanter config    # Show resolved configuration
enchanter prompt    # Show assembled system prompt
```

### Session summaries

When you exit the REPL with `/exit` or Ctrl+D, Enchanter generates a concise summary of your session and saves it to memory. Your next session automatically loads this context, so you can pick up where you left off.

- Summaries are skipped for single-shot mode (`-p` flag)
- Skipped if the session was too short (no real exchange)
- Timeout of 10 seconds; falls back to a simple message count on failure
- Disable with `summarize_on_exit: false` in the `agent` section of config.yaml

> **Ctrl+C is a force-quit.** It bypasses the exit hook, so no session summary is saved. Use `/exit` or Ctrl+D for a clean exit.

## How it works

The system prompt is built in three layers:

1. **SOUL** — your persona from SOUL.md, stable across turns
2. **CONTEXT** — environment info, skills index, tool guidance
3. **VOLATILE** — memory, user profile, timestamp

Memory uses the same `§`-delimited format as Hermes Agent. Skills use the
same SKILL.md format (agentskills.io). If you're coming from Hermes, just
copy or symlink your data — the structure matches.

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

## License

MIT — Copyright 2026 Andrew S Erwin