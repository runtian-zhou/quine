---
status: pending
---

# Fix Enter Key to Send Message in Interactive CLI

## Overview

The interactive TUI chat currently uses Enter to insert a newline and Ctrl+S to submit input (introduced in feature 014). This is unintuitive — virtually all chat interfaces use Enter to send and Shift+Enter (or similar) to insert newlines. Users expect Enter to send their message.

## Requirements

### 1. Key Binding Changes (`crates/quine-cli/src/tui/mod.rs`)

Swap the Enter/newline keybindings:

| Key Combo | Current Behavior | New Behavior |
|-----------|-----------------|--------------|
| `Enter` | Insert newline | **Submit input** |
| `Shift+Enter` | N/A | **Insert newline** |
| `Ctrl+S` | Submit input | Submit input (keep as secondary binding) |

In `handle_terminal_event` (line ~176), change the `KeyCode::Enter` match arm:

```rust
KeyCode::Enter => {
    if modifiers.contains(KeyModifiers::SHIFT) {
        // Shift+Enter inserts a newline.
        app.input.insert_newline();
        None
    } else {
        // Plain Enter submits (both normal and interaction mode).
        app.submit_input()
    }
}
```

**Terminal compatibility note:** `Shift+Enter` may not be reliably detected in all terminals. As a fallback, keep `Ctrl+S` as an alternative submit binding and consider also supporting `Alt+Enter` for newline insertion:

```rust
// Alt+Enter also inserts newline (fallback for terminals that don't emit Shift+Enter).
if code == KeyCode::Enter && modifiers.contains(KeyModifiers::ALT) {
    app.input.insert_newline();
    return None;
}
```

### 2. Help Text Update (`crates/quine-cli/src/tui/ui.rs`)

If there is any status bar or hint text showing keybindings, update it to reflect:
- `Enter` to send
- `Shift+Enter` for new line
- `Ctrl+S` to send (alternative)

### 3. No Changes to `InputBuffer` (`crates/quine-cli/src/tui/app.rs`)

The `InputBuffer` struct and its methods are correct. Only the key event routing in `mod.rs` needs to change.

## Acceptance Criteria

- `cargo build` / `cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --all -- --check` must all pass
- Pressing Enter in the input box submits the message (same as old pre-014 behavior)
- Pressing Shift+Enter (or Alt+Enter) inserts a newline for multi-line input
- Ctrl+S continues to work as an alternative submit binding
- Interaction/permission prompts: Enter still submits (unchanged)
- Existing tests must continue to pass
- New/updated unit tests:
  - `enter_submits_input` — plain Enter submits the message
  - `shift_enter_inserts_newline` — Shift+Enter inserts a newline
  - `alt_enter_inserts_newline` — Alt+Enter inserts a newline (fallback)
  - `ctrl_s_still_submits` — Ctrl+S continues to submit

## QA Test Cases (add to `qa/test_cases.json`)

```json
[
  {
    "name": "tui_enter_sends_message",
    "description": "Enter key sends the message instead of creating a new line",
    "steps": [
      "Launch TUI with `cargo run --bin quine -- chat`",
      "Type 'hello world'",
      "Press Enter",
      "Verify the message is submitted (appears in conversation, agent starts processing)"
    ]
  },
  {
    "name": "tui_shift_enter_newline",
    "description": "Shift+Enter inserts a newline for multi-line input",
    "steps": [
      "Launch TUI",
      "Type 'line one'",
      "Press Shift+Enter",
      "Type 'line two'",
      "Press Enter to submit",
      "Verify the submitted message contains both lines"
    ]
  },
  {
    "name": "tui_alt_enter_newline_fallback",
    "description": "Alt+Enter also inserts a newline (terminal compatibility fallback)",
    "steps": [
      "Launch TUI",
      "Type 'line one'",
      "Press Alt+Enter",
      "Type 'line two'",
      "Press Enter to submit",
      "Verify the submitted message contains both lines"
    ]
  }
]
```

## Non-Goals (Deferred)

- Configurable keybindings (user preference for Enter vs Shift+Enter behavior)
- Detecting terminal capabilities for Shift+Enter support
- Any changes to the `InputBuffer` data structure
