# Changelog

All notable changes to enchanter are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Session resume: reload prior conversation history and continue where you left off
- Tool result cache: avoid redundant tool executions within a session
- Custom provider headers: pass arbitrary HTTP headers to LLM API providers
- TUI mode re-introduced with `--tui` CLI flag (multi-pane layout, streaming, thinking indicator, Ctrl+HJKL/Ctrl+Arrows pane navigation)
- Test suite expanded to 184 tests (focus navigation, chat scroll, list tests)

### Changed
- Migrated from unmaintained `serde_yml` crate to `serde_yaml` 0.9

### Fixed
- `exec_command` now enforces a real timeout instead of blocking indefinitely
- UTF-8 panics on truncated multi-byte output in `exec_command` resolved
- Daemon: fixed system prompt and model config leaking across sessions
- Daemon: system prompt override now correctly applied in daemon mode
- TUI: chat pane auto-scroll, scroll direction, and clamping fixes
- TUI: defer quit while streaming to preserve session save path
- TUI: session names displayed correctly
- MCP tool name prefix: colons replaced with double underscores to avoid parsing issues
- Status bar prints inline above prompt instead of using absolute cursor positioning

---

## [0.4.3] - 2026-06-10

### Added
- crossterm-based REPL with pinned bottom status bar and stuck-query detection
- Persistent activity log for hang diagnosis
- Real-time context token tracking displayed in status bar
- Provider compatibility documentation with tool calling support table

### Fixed
- Shifted characters (e.g. `!@#$`) accepted correctly in REPL input

---

## [0.4.2] - 2026-06-09

### Added
- Structured knowledge store (kstore): persistent key-value facts with auto-capture via prompt instructions
- Project-level knowledge overlay (kstore Phase 3)
- Unlimited agent turns when `max_turns` is set to 0

### Fixed
- `base_url` documentation corrected: it is the full chat completions endpoint URL
- Multi-byte UTF-8 truncation panic resolved (`floor_char_boundary`)
- Timeouts added to LLM API client to prevent hanging connections
- Suppressed `dead_code` warning on `Source::as_str`

---

## [0.4.1] - 2026-06-04

### Added
- `enchanter init` subcommand: scaffolds a project `.enchanter/` overlay directory
- Project overlay wired into startup (global config is truth, project is additive)
- Landlock filesystem sandbox (Linux only) — restricts file access to declared paths
- Context compaction to manage conversation length
- LLM schema validation on response

### Changed
- Deduplicated agent loop: extracted `EventSink`, collapsed two identical loops

### Fixed
- Code review fixes applied across the codebase
- Removed stale `enchanter` directory from repo root

---

## [0.3.0] - 2026-05-27

### Added
- Optional TUI mode with multi-pane layout (chat, tools, memory, plan panes)
- TUI memory management and pane highlighting
- TUI keybindings polish, auto-scroll, and streaming improvements
- Session summary displayed on exit

### Fixed
- Daemon mode: proper double-fork daemonization and responsive signal handling
- Daemon now correctly runs in background (not attached to terminal)

### Changed
- Technical polish pass and code audit

---

## [0.2.0] - 2026-05-26

### Added
- Session recording and replay (`enchanter rec` / `enchanter replay`)
- Prompt diff and budget inspection (`enchanter insp`)
- Cross-platform support: daemon mode gated behind `cfg(unix)` for macOS compatibility
- Software Requirements Specification published for community review

---

## [0.1.0] - 2026-05-25

### Added
- **Core REPL**: interactive line-oriented REPL with streaming responses
- **REPL commands**: `/model`, `/retry`, `/undo`, `/tools`, `/bye`, `/quit`
- **Tool system**: 7 built-in tools (shell exec, file read/write/edit, search, memory store/retrieve, memory list)
- **MCP client**: Model Context Protocol support with stdio transport, server discovery, tool dispatch, and lifecycle management
- **MCP HTTP transport**: remote MCP server support
- **MCP server auto-restart**: crashed MCP servers are automatically restarted
- **Named providers**: `/model` command to switch between configured LLM providers mid-session
- **Memory management**: conversation memory with cap + summarization
- **Session persistence**: Phase 1 of daemon mode (save/load session state)
- **Daemon mode**: background process with warm MCP servers
- **Session summary**: displayed on REPL exit
- **Soft turn limit**: nudges model to wrap up before hard cutoff
- **Configurable turn limit**: default changed to 60
- **Attribution comments**: comprehensive credits for borrowed patterns
- **Software Requirements Specification** document for community review

### Fixed
- Streaming SSE `[DONE]` handling and tool call accumulation
- MCP client robustness hardened
- `floor_char_boundary` panic on string slicing

---

## [0.0.1] - 2026-05-23 (Initial release)

### Added
- Initial project structure: Cargo workspace, README, MIT License
- Basic CLI skeleton with OpenAI-compatible streaming chat
- Social preview image

---

[Unreleased]: https://github.com/andrewthecoder/enchanter/compare/v0.4.3...HEAD
[0.4.3]: https://github.com/andrewthecoder/enchanter/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/andrewthecoder/enchanter/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/andrewthecoder/enchanter/compare/v0.3.0...v0.4.1
[0.3.0]: https://github.com/andrewthecoder/enchanter/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/andrewthecoder/enchanter/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/andrewthecoder/enchanter/compare/v0.0.1...v0.1.0
[0.0.1]: https://github.com/andrewthecoder/enchanter/releases/tag/v0.0.1
