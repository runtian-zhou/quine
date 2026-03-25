---
status: done
---

# Interactive CLI TUI

Redesign `quine chat` as a terminal UI with a persistent input box, streaming animations, collapsible tool output, and queued interaction handling.

## Layout

```
┌──────────────────────────────────────────┐
│  Scrollable conversation view            │
│  ─ user messages                         │
│  ─ assistant text (streamed)             │
│  ─ tool calls (collapsed by default)     │
│  ─ interaction prompts (highlighted)     │
│                                          │
├──────────────────────────────────────────┤
│ > input box (always visible)             │
└──────────────────────────────────────────┘
```

- Conversation view: scrollable (Up/Down, Page Up/Down), line-wrapped to terminal width, auto-scrolls to bottom on new output unless user scrolled up.
- Input box: pinned at bottom, always accepts input. Enter sends message or responds to pending interaction.

## Waiting Animation

- Show spinner while LLM is processing (e.g. `⠋ Thinking...` cycling braille frames).
- Replace spinner with streamed text once deltas arrive.
- During tool execution show `⠋ Running tool: bash...` with tool name.

## Tool Output

Collapsed by default:
```
▶ bash: echo hello
```

Expanded (toggle with Enter):
```
▼ bash: echo hello
  ┃ hello
```

- Show tool name + first ~60 chars of command/path.
- Tool results dimmed/styled differently from assistant text.

## Interaction Queue

- `interaction_needed` notifications queued, presented one at a time.
- Input box label changes: `[ask_user] What is your name? > `.
- Submitting a response sends `submit_interaction_response`, then shows next queued interaction.
- Badge shows pending count: `[2 pending]`.
- When queue is empty, input reverts to normal message mode.

## Key Bindings

| Key | Action |
|-----|--------|
| Enter | Send message / submit interaction response |
| Ctrl-C | Cancel current operation or quit |
| Ctrl-D | Quit |
| Up/Down | Scroll conversation |
| Page Up/Down | Fast scroll |
| Escape | Cancel input / dismiss |

## Implementation

- Use `ratatui` + `crossterm` for TUI rendering.
- New `TuiRenderer` implementing the existing `Renderer` trait.
- Restructure chat event loop as `select!` over: terminal events (`crossterm::event::EventStream`), daemon notifications (`client.recv_notification()`), spinner tick (~80ms).
- Keep `TerminalRenderer` as fallback for non-TTY (piped input).
- Add `ratatui` and `crossterm` to `quine-cli/Cargo.toml`.

## Acceptance Criteria

- `quine chat` launches TUI with described layout.
- Spinner animates while waiting for LLM.
- Tool calls collapsed by default, expandable.
- Interaction prompts queued and presented one at a time.
- Line wrapping and scrolling work.
- Non-TTY falls back to plain text renderer.
- All CI checks pass.
