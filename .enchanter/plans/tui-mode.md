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

### Task 1: Scaffold the TUI module

- Add `ratatui` and `crossterm` as optional dependencies gated by `tui` feature
- Create `src/tui/mod.rs` with `TuiApp` struct that holds `AgentSession` and
  terminal state
- Add `Tui` subcommand to `Commands` enum (gated by `#[cfg(feature = "tui")]`)
- Create the main event loop: terminal setup → render loop → teardown
- Wire into `cli::run()` so `enchanter tui` launches the TUI

```rust
// src/tui/mod.rs
pub struct TuiApp {
    agent: AgentSession,
    // Terminal state
    // Pane states
    // Input state
}

impl TuiApp {
    pub async fn run(mut self) -> Result<()> { ... }
}
```

### Task 2: Define pane types and focus management

- Create `src/tui/panes.rs` with enum for pane types and focus cycling
- Each pane tracks its own scroll position and selection
- Focus ring: Skills → Memory → Chat → Input, cycling with Tab

```rust
#[derive(Clone, Copy, PartialEq)]
pub enum Pane {
    Skills,
    Memory,
    Chat,
    Input,
}
```

### Task 3: Implement app state

- Create `src/tui/app.rs` with `App` struct holding all runtime state:
  - `AgentSession` (the core)
  - Current focus pane
  - Chat history (messages rendered as lines)
  - Streaming state (current response being streamed)
  - Scroll positions per pane
  - Input buffer
  - Status info (model, tool count, etc.)

### Task 4: Terminal setup and teardown

- Raw mode setup via crossterm
- Alternate screen buffer
- Proper restore on exit (even on panic)
- Use `ratatui::Terminal<Backend>` with crossterm backend

### Task 5: Render the status bar / header

- Top bar: app name, model, session ID short, tool count
- Bottom bar: keybinding hints, turn number, token info
- These are always visible regardless of focus

### Task 6: Render the skills pane

- List skills grouped by category
- Highlight selected skill
- Show description on selection
- Mark focus with a border color change

### Task 7: Render the memory pane

- List memory entries (truncated, scrollable)
- Highlight selected entry
- Show full content in a popup or chat pane on Enter

### Task 8: Render the chat pane

- Display conversation history with role indicators
- Streaming text support (token-by-token updates)
- Tool call blocks with collapsible results
- Auto-scroll during streaming, manual scroll otherwise

### Task 9: Render the input bar

- Text input with cursor
- `/` command support (same commands as REPL)
- Multi-line mode toggle (Ctrl+Enter vs Enter)

### Task 10: Event loop integration

- Use `chat_events()` API to stream LLM responses into the TUI
- `tokio::select!` between:
  - crossterm terminal events (key presses, resize)
  - LLM streaming events (content, tool calls, done)
  - MCP events
- On each event, update state and re-render

### Task 11: Slash command handling in TUI

- Parse `/` commands identically to REPL
- Some commands update panes (e.g., `/model` refreshes status bar)
- Unknown commands show error inline

### Task 12: Feature gate and CI

- Ensure `cargo build` (without `tui` feature) still compiles
- Ensure `cargo build --features tui` builds with TUI
- Default features include `tui`
- Update CI/workflows if needed

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