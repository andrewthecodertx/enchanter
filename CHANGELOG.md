# Changelog

All notable changes to enchanter are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.1] - 2026-08-31

Security and robustness release, closing the findings from the v1.0.0
security code review.

### Added
- **Tool approval mode** — `security.require_tool_approval: true` asks the
  user before running dangerous tools (`exec_command`, `write_file`,
  `edit_file`, `memory`, `knowledge`, and all MCP tools). REPL prompts
  y/N; web UI renders Approve/Reject cards. Read-only tools are exempt.
  Fail-closed: no approval channel, timeout, or rejection denies the call.
- **Optional web UI token auth** — `enchanter serve --token <TOKEN>` (or
  `web.auth_token` in config) requires `Authorization: Bearer` on all
  `/api/*` routes. SSE endpoints also accept `?token=`. Loopback-only
  default unchanged when no token is set.
- **API retry/backoff** — transient failures (429, 5xx, connect/read
  timeouts) retry with exponential backoff plus jitter, honoring
  `Retry-After`. Configurable via `agent.retry` (`max_attempts`,
  `base_delay_ms`, `max_delay_ms`). 4xx client errors are never retried.
- **`docs/security.md`** — full threat-model page: Landlock scope and
  limits, non-Linux fallback semantics, approval mode, web auth, secrets
  handling, prompt-injection hardening, timeouts, token estimation.

### Changed
- `exec_command` timeout (30s) now kills the child's **process group** on
  Unix so `sh -c` cannot orphan background children. Timeout value is a
  named constant.
- `security.require_tool_approval` field added (default `false` — no
  behavior change).

### Security
- Human-in-the-loop gate for dangerous tools (opt-in).
- Optional shared-secret auth for the web UI (loopback default unchanged).
- Process-group cleanup on exec timeout.

## [1.0.0] - 2026-08-28

First stable release. Web UI, setup wizard, token usage tracking, and
cross-platform prebuilt binaries.

### Added
- **Web UI** — `enchanter serve` starts a local web interface (default `http://127.0.0.1:3005`) with streaming responses, tool-call visibility in a side panel, model switching, session resume/rename, and token/context display. Replaces the ratatui TUI.
- **First-run setup wizard** — `enchanter` offers an interactive setup when no model/key is configured (provider presets, masked API key entry, guided config). `enchanter config --edit` and `enchanter config --set key=value` for non-interactive editing.
- **Token usage tracking** — provider-reported usage captured per turn and modeled session-total; shown in the REPL status bar, `/ctx`, `/cost`, and the web UI. Falls back to character-based estimation when providers omit usage.
- **Model info** — `enchanter models` queries the active provider for its real model list and context windows; `context_window` config field overrides the built-in table.
- **Session titles** — sessions get auto-derived titles (first user message); rename via web UI or API.
- **Prebuilt binaries** — GitHub Actions release workflow builds native binaries for Linux, macOS, and Windows on x86_64 and arm64, attached to the release.
- **Tool side panel** — web UI shows tool calls (arguments + results) in a left-hand panel instead of inline with the conversation.

### Changed
- Switched default web port to 3005 (`enchanter serve --port` to override).
- REPL status bar hardened to avoid fighting terminal input echo (scrolls as a regular line).

### Removed
- TUI mode (`--tui`, ratatui/crossterm) — superseded by the web UI.

## [0.9.1] - 2026-08-21

First public release since v0.4.3. Rolls up all work that had accumulated in
`[Unreleased]` (the in-tree version sat at 0.5.0 but was never released) plus
a round of critical-bug fixes and cleanup.

### Added
- Session resume: `--resume <id>` reloads prior conversation history and continues where you left off
- Tool result cache: avoids redundant read-only tool executions within a session
- Custom provider headers: pass arbitrary HTTP headers to LLM API providers
- TUI mode re-introduced with `--tui` CLI flag (multi-pane layout, streaming, thinking indicator, Ctrl+HJKL/Ctrl+Arrows pane navigation)
- `config_version` schema field with version checking in config loading
- Daemon: SIGHUP triggers a graceful restart (stops accepting, cleans up, exits with code 0) so `daemon start` after a signal picks up new config
- Test suite expanded to 177 tests (focus navigation, chat scroll, list tests)

### Changed
- Migrated from unmaintained `serde_yml` crate to `serde_yaml` 0.9
- Removed the prompt diff feature (`PromptLayers::diff`, `PromptDiffResult`, `LayerChange`, `format_diff`, the `similar` crate dependency, and the `prompt --diff` / `/prompt diff` surfaces). Budget inspection via `prompt --budget` / `/prompt budget` is unchanged. May be re-added later if the feature earns its keep.
- Flattened single-file module folders: `src/{config,soul,memory,skills,api,cli,tools,mcp,kstore}/mod.rs` → `src/<name>.rs`

### Fixed
- **Security:** `write_file` and `search_files` rejected `..` path components and patterns containing `/`, `\`, or `..` (directory traversal)
- **Daemon:** one-shot `-p` calls are now stateless per request — no conversation history or tool-cache bleed between calls (matches inline `-p` behavior)
- **Daemon:** system prompt and model config no longer leak across sessions; `-s/--system` override now applied in daemon mode
- **Daemon:** SIGHUP handled explicitly (previously a raw `SIG_IGN`)
- **Session resume:** consecutive tool calls are grouped into a single assistant message so resumed history matches the API's expected shape
- `exec_command` now enforces a real timeout instead of blocking indefinitely
- UTF-8 panics on truncated multi-byte output in `exec_command` resolved
- TUI: chat pane auto-scroll, scroll direction, and clamping fixes
- TUI: defer quit while streaming to preserve session save path
- TUI: session names displayed correctly
- MCP tool name prefix: colons replaced with double underscores to avoid parsing issues
- Status bar prints inline above prompt instead of using absolute cursor positioning

### Removed
- `similar` crate dependency (was only used by the removed prompt diff feature)
- Sandbox integration tests (covered by the path-traversal unit tests)

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

[Unreleased]: https://github.com/andrewthecodertx/enchanter/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/andrewthecodertx/enchanter/compare/v0.9.1...v1.0.0
[0.9.1]: https://github.com/andrewthecodertx/enchanter/compare/v0.4.3...v0.9.1
[0.4.3]: https://github.com/andrewthecodertx/enchanter/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/andrewthecodertx/enchanter/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/andrewthecodertx/enchanter/compare/v0.3.0...v0.4.1
[0.3.0]: https://github.com/andrewthecodertx/enchanter/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/andrewthecodertx/enchanter/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/andrewthecodertx/enchanter/compare/v0.0.1...v0.1.0
[0.0.1]: https://github.com/andrewthecodertx/enchanter/releases/tag/v0.0.1
