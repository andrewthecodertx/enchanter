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

| Command    | What it does                |
|------------|-----------------------------|
| `/help`    | Show available commands     |
| `/clear`   | Reset conversation history  |
| `/soul`    | Show SOUL.md content        |
| `/memory`  | Show loaded memory           |
| `/skills`  | List discovered skills       |
| `/config`  | Show resolved configuration |
| `/prompt`  | Show full system prompt     |
| `/exit`    | Quit                        |

### Info subcommands

```bash
enchanter soul      # Show current SOUL.md
enchanter memory    # Show loaded memory
enchanter skills    # List discovered skills
enchanter config    # Show resolved configuration
enchanter prompt    # Show assembled system prompt
```

## How it works

The system prompt is built in three layers:

1. **SOUL** — your persona from SOUL.md, stable across turns
2. **CONTEXT** — environment info, skills index, tool guidance
3. **VOLATILE** — memory, user profile, timestamp

Memory uses the same `§`-delimited format as Hermes Agent. Skills use the
same SKILL.md format (agentskills.io). If you're coming from Hermes, just
copy or symlink your data — the structure matches.

## License

MIT — Copyright 2026 Andrew S Erwin