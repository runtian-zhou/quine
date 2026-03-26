---
status: pending
---

# Esc to Cancel In-Flight Agent Work

## Overview

Pressing Esc in the TUI currently only clears the input buffer. It should also cancel in-flight agent work (LLM streaming, tool execution, bash commands) when the agent is not idle. The cancel infrastructure already exists through the full stack (`CoreInput::Cancel`, `HarnessService::cancel`, `methods::CANCEL` RPC) — it just needs to be wired into the TUI's Esc key handler.

## Requirements

### 1. Add `Cancel` variant to `AppAction` (`crates/quine-cli/src/tui/app.rs`)

```rust
pub enum AppAction {
    SendMessage(String),
    SubmitInteraction(String),
    Cancel,
    Quit,
}
```

### 2. Wire Esc key to cancel when agent is busy (`crates/quine-cli/src/tui/mod.rs`)

Update the `KeyCode::Esc` handler in `handle_terminal_event`:

```rust
KeyCode::Esc => {
    if app.is_selecting_options() {
        app.option_select = None;
        None
    } else if app.phase != AgentPhase::Idle {
        // Agent is busy — cancel in-flight work.
        Some(AppAction::Cancel)
    } else {
        // Agent is idle — clear input buffer.
        app.input.clear();
        None
    }
}
```

Priority: option-select dismissal > cancel busy agent > clear input.

### 3. Execute cancel action (`crates/quine-cli/src/tui/mod.rs`)

Add a `Cancel` arm to `execute_action`:

```rust
AppAction::Cancel => {
    let params = serde_json::json!({
        "session_id": app.session_id,
    });
    if let Err(e) = client.call(methods::CANCEL, Some(params)).await {
        app.messages.push(app::ConversationEntry::Error(e.to_string()));
    }
    app.phase = AgentPhase::Idle;
}
```

### 4. Show cancellation feedback in conversation view (`crates/quine-cli/src/tui/app.rs`)

When a cancel is issued, push a visual indicator to the conversation:

In `apply_notification` for `SESSION_STATE_CHANGED`, if the state transitions to `Idle` while the current phase is not `Idle`, and the streaming buffer has partial content, flush it and add a dimmed `(cancelled)` indicator.

Alternatively, handle this in `execute_action` right after sending the cancel:
```rust
app.messages.push(ConversationEntry::Error("(cancelled)".into()));
app.streaming_buffer.clear();
```

### 5. Plain chat mode (`crates/quine-cli/src/chat.rs`)

The plain chat mode uses Ctrl+C to interrupt (line 94). No changes needed — Ctrl+C already works via `tokio::signal::ctrl_c()` in the inner notification loop. The Esc key is not available in non-raw-mode terminals.

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass
- Esc during streaming cancels the LLM response and returns to idle
- Esc during tool execution cancels and returns to idle
- Esc while idle clears the input buffer (unchanged)
- Esc during option selection dismisses the selector (unchanged)
- Partial streaming buffer is flushed on cancel
- A `(cancelled)` indicator appears in the conversation view
- Existing tests continue to pass
- New unit tests:
  - `esc_cancels_when_streaming` — verify AppAction::Cancel is returned when phase is Streaming
  - `esc_clears_input_when_idle` — verify input is cleared and no action returned when idle
  - `esc_dismisses_options_first` — verify option select is dismissed before cancel

## QA Test Cases

```json
[
  {
    "name": "esc_cancels_streaming",
    "description": "Pressing Esc during LLM streaming cancels the response",
    "steps": [
      "Start TUI chat",
      "Send a message that triggers a long response",
      "While the response is streaming, press Esc",
      "Verify streaming stops and (cancelled) appears",
      "Verify the input prompt returns to idle state"
    ]
  },
  {
    "name": "esc_cancels_tool_execution",
    "description": "Pressing Esc during tool execution cancels it",
    "steps": [
      "Send a message that triggers a long-running bash command",
      "While the tool is running, press Esc",
      "Verify the tool execution stops and agent returns to idle"
    ]
  },
  {
    "name": "esc_clears_input_when_idle",
    "description": "Pressing Esc when idle clears the input buffer",
    "steps": [
      "Type some text in the input box",
      "Press Esc",
      "Verify the input is cleared but no cancel is sent"
    ]
  }
]
```

## Non-Goals (Deferred)

- Cancel confirmation dialog ("Are you sure?")
- Partial result recovery after cancel
- Cancel individual tool calls within a multi-tool turn
- Cancel child/subagent sessions when parent is cancelled
