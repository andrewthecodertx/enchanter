# TUI Mode for Enchanter

## Overview

Add an optional terminal user interface (TUI) mode that provides a rich,
multi-pane, keyboard-driven experience similar to lazygit. The existing REPL
remains fully functional and is the default; the TUI is activated via
`enchanter tui`.

## Architecture Decisions

### Framework: `ratatui` + `crossterm`

**ratatui** is the clear choice. It powers lazygit, is mature (v0.29+),
well-documented, and has a thriving ecosystem. crossterm provides cross-platform
terminal handling. No credible alternatives exist at this quality level.

### Compile-time feature gate

The TUI adds meaningful dependencies (ratatui, crossterm, maybe tui-widgets).
Cargo feature `tui` makes it optional so minimal users don't pay the cost.
Default features include it so `cargo install enchanter` gets both modes.

```toml
[features]
default = ["tui"]
tui = ["dep:ratatui", "dep:crossterm"]
```

### Activation: `enchanter tui` subcommand

The REPL is default behavior (no subcommand = REPL). `enchanter tui` launches
the TUI. `--no-tui` flag is not needed since the default is REPL.

## Pane Layout

The TUI presents a dense, scannable layout inspired by lazygit's split-screen
design. All panes are keyboard-navigable using vim-style hjkl or arrow keys.

```
┌──────────────────────────────────────────────────────────────────┐
│ Enchanter v0.1 │ model=claude-sonnet │ session=a1b2c3d4 │ 88t │
├────────────────┬─────────────────────────────────────────────────┤
│  SKILLS (6)    │  CHAT                                          │
│  ────────────  │  ─────────────────────────────────────────────  │
│ ▸ research     │  Assistant: Let me analyze the codebase and    │
│   arxiv        │  identify the key architectural patterns...    │
│ ▸ softdev      │                                                │
│   blog-publish │  ┌─ TOOL: exec_command ──────────────────────┐  │
│ ▸ creative     │  │ $ grep -rn "fn main" src/                 │  │
│   img-gen      │  │ src/main.rs:83:fn main() {               │  │
│                │  └──────────────────────────────────────────┘  │
│  ── MEMORY ──  │                                                │
│  [1] Always    │  > type your message here...                   │
│  [2] Rust 1.85 │                                                │
│  [3] Overlay   │                                                │
├────────────────┼─────────────────────────────────────────────────┤
│  STATUS BAR: tools=24 (6 MCP) │ tokens≈4200 │ turn 3 │ ↑↓←→ │
└────────────────┴─────────────────────────────────────────────────┘
```

### Left sidebar (30% width, two stacked panes)

1. **Skills pane** — lists discovered skills grouped by category, with
   descriptions. Selectable: pressing Enter on a skill injects its content
   into the chat input as a context reference.

2. **Memory pane** — shows loaded memory entries. Scrollable. Selectable:
   pressing Enter on an entry shows full content in the chat pane.

### Main area (70% width)

3. **Chat pane** — the primary interaction area. Shows conversation history
   (user messages, assistant responses, tool calls/results). Streaming tokens
   appear in real-time. Scrollable with PgUp/PgDn.

4. **Input bar** — at the bottom of the chat pane. Single-line or multi-line
   input (Ctrl+Enter to send, Enter for newline in multi-line mode). Supports
   `/` commands like the REPL.

### Bottom status bar

5. **Status bar** — shows model, provider, tool count, token budget usage,
   turn number, connection status, and keybinding hints.

## Keyboard Controls

```
Tab / Shift+Tab    Cycle focus between panes
↑↓ / j/k           Navigate items in focused pane
Enter              Activate item / send message
PgUp/PgDn          Scroll chat history
Ctrl+C             Cancel current request
Ctrl+Q / q         Quit
1-5                 Jump to pane by number
/                   Focus input bar
Esc                 Unfocus / return to previous pane
?                   Help overlay
```

## Implementation Tasks

### Task 1: Scaffold the TUI module ✅

- Add `ratatui` and `crossterm` as optional dependencies gated by `tui` feature
- Create `src/tui/mod.rs` with event loop, terminal setup/teardown
- Add `Tui` subcommand to `Commands` enum (gated by `#[cfg(feature = "tui")]`)
- Wire into `cli::run()` so `enchanter tui` launches the TUI

### Task 2: Define pane types and focus management ✅

- Pane enum in `src/tui/app.rs` with focus cycling (next/prev)
- Focus ring: Skills → Memory → Chat → Input, cycling with Tab/Shift+Tab
- Number keys 1-4 jump to specific panes

### Task 3: Implement app state ✅

- `App` struct in `src/tui/app.rs` holding AgentSession, focus, chat lines, input, streaming state
- `ChatLine` enum for different message types
- `InputState` with cursor, buffer, multiline mode

### Task 4: Terminal setup and teardown ✅

- Raw mode via crossterm, alternate screen buffer
- Panic hook to restore terminal on crash
- Clean teardown on normal exit

### Task 5: Render the status bar / header ✅

- Header: app name, model, provider, session ID, turn count
- Footer: tool count, MCP count, skills count, streaming indicator, keybinding hints

### Task 6: Render the skills pane ✅

- List skills with category tags
- Highlight selected skill with ratatui ListState
- Enter shows skill details in chat pane
- Focus-dependent border color

### Task 7: Render the memory pane ✅

- USER and NOTES sections with separate styling
- Highlighted selected entry
- Auto-scroll to keep selection visible
- Enter shows full content in chat pane

### Task 8: Render the chat pane ✅

- User messages (⟩ prefix), assistant responses (⟨ prefix)
- Tool calls (⟩ prefix, yellow), tool results (│ prefix, dimmed, max 5 lines)
- Streaming text with cursor indicator ▌
- Auto-scroll during streaming, manual scroll with PgUp/PgDn
- End key re-enables auto-scroll

### Task 9: Render the input bar ✅

- Single-line input with cursor
- Slash command support (mirrors all REPL commands)
- Ctrl+M toggles multiline mode (Enter=newline, Ctrl+Enter=send)
- Ctrl+A/E for home/end, Ctrl+U to clear line
- Home/End key support

### Task 10: Event loop integration ✅

- `chat_events()` API streams LLM responses into TUI via UnboundedReceiver<Event>
- Non-blocking `try_recv()` poll during streaming
- 16ms sleep between renders during streaming
- Ctrl+C cancels streaming, Ctrl+Q quits
- Memory management after each turn completes

### Task 11: Slash command handling in TUI ✅

- All REPL slash commands mirrored in `commands.rs`
- /help, /clear, /soul, /memory, /skills, /tools, /config, /prompt, /prompt diff, /prompt budget
- /model <name> switches model and refreshes status
- /sessions lists history
- /retry and /undo work with streaming
- Unknown commands show error inline

### Task 12: Feature gate and CI ✅

- `cargo build --features tui` compiles with TUI
- `cargo build --no-default-features` compiles REPL-only
- All 88 tests pass in both configurations
- Default features include `tui`

### Bonus features implemented:

- Session summary on exit (matching REPL behavior)
- `/` key focuses input pane and starts a command
- `?` shows keybinding help in any pane
- Esc returns focus to input from other panes
- Window resize event handling
- Memory management after each streaming turn

## File Structure

```
src/tui/
├── mod.rs         — Public interface, TuiApp, entry point
├── app.rs         — Application state (App struct)
├── panes.rs       — Pane type definitions, focus management
├── render.rs      — All ratatui rendering (draw methods)
├── input.rs       — Input handling (key events, text editing)
└── commands.rs    — Slash command handler (shared logic with REPL)
```

## Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| ratatui API churn | Pin to stable 0.29.x; ratatui is mature now |
| Terminal compatibility | crossterm handles cross-platform; test on Linux, macOS, Windows Terminal |
| Streaming performance | Debounce renders to ~60fps max; skip frames during rapid token bursts |
| TUI state diverges from REPL | Share `AgentSession`; TUI is just a different view on same state |
| Panic during raw mode | Use `std::panic::set_hook` to restore terminal before exiting |

## Alternatives Considered

1. **ink (Rust TUI via React-like model)** — Interesting but less mature ecosystem, fewer examples, harder to build custom layouts. ratatui's imperative model is simpler for our layout needs.

2. **cursive** — Older, less maintained, uses ncurses backend (not crossterm). No real advantage over ratatui for our use case.

3. **Always-on TUI (no REPL)** — Rejected per requirements. REPL must stay.

4. **Runtime flag instead of compile-time feature** — Adds deps for everyone even if they never use TUI. Feature gate is cleaner. Default-on means most users get it with no extra step.