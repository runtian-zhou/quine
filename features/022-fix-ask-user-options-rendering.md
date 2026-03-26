---
status: pending
---

# Fix Ask-User Options Rendering in TUI

## Overview

The ask_user tool with options (SingleSelect/MultiSelect) does not render the interactive option selector in the TUI. Two bugs cause this:

1. **Server drops options from notification**: In `crates/quine-harness/src/server.rs`, the `interaction_needed` notification only serializes `prompt` and `kind` from the `InteractionRequest` — it does **not include `options` or `allow_freeform`**. The TUI therefore receives an empty options array and never enters option-select mode.

2. **Options display in conversation as Error**: When options do arrive (once bug 1 is fixed), the prompt and numbered options list are pushed to the conversation view as `ConversationEntry::Error`, which renders in red. This should use a dedicated style or at least not look like an error.

## Requirements

### 1. Include `options` and `allow_freeform` in notification (`crates/quine-harness/src/server.rs`)

In the `core_output_to_notification` (or equivalent log/notification function), the `InteractionNeeded` arm currently serializes:

```rust
serde_json::json!({
    "prompt": request.prompt,
    "kind": request.kind,
})
```

Change to:

```rust
serde_json::json!({
    "prompt": request.prompt,
    "kind": request.kind,
    "options": request.options,
    "allow_freeform": request.allow_freeform,
})
```

This ensures the TUI receives the full `SelectOption` list and can build `OptionSelectState`.

### 2. Fix option display in conversation view (`crates/quine-cli/src/tui/app.rs`)

When a `SingleSelect` or `MultiSelect` interaction arrives, the prompt+options are currently pushed as `ConversationEntry::Error(label)`. Change this to push as a non-error entry. Options:

- Use a new `ConversationEntry::Interaction(String)` variant that renders in cyan/yellow instead of red, OR
- Continue using the existing `Error` variant but style it differently in `ui.rs` when the text starts with `❓`

The simpler approach: use a dedicated conversation entry or just change the style of the existing error-based rendering to not look like an error (e.g., cyan for ask-user prompts).

### 3. Verify option selector activates (`crates/quine-cli/src/tui/app.rs`)

Once options are included in the notification, verify that:
- `OptionSelectState` is created when `is_select && self.interaction_queue.len() == 1`
- The input area switches to option-select rendering mode
- Up/Down/Enter/Space keys work for navigation and selection

No code changes expected here — this should work automatically once bug 1 is fixed.

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass
- The `interaction_needed` JSON-RPC notification includes `options` and `allow_freeform` fields
- TUI enters option-select mode when ask_user provides options
- Options render with `›` cursor and `[x]` checkboxes (existing rendering in `ui.rs`)
- The conversation prompt for select interactions does not render as a red error
- Existing tests continue to pass
- New unit test: verify `core_output_to_notification` includes options in InteractionNeeded

## QA Test Cases

```json
[
  {
    "name": "ask_user_options_rendered",
    "description": "Verify ask_user with options shows interactive selector in TUI",
    "steps": [
      "Start TUI chat session",
      "Send: Use ask_user to ask 'Pick a color' with options: red, green, blue",
      "Verify the input area shows an option selector with › cursor",
      "Press Down to highlight 'green', press Enter",
      "Verify the response 'green' is submitted"
    ]
  },
  {
    "name": "ask_user_options_not_error_style",
    "description": "Verify option prompt does not render in red error style",
    "steps": [
      "Trigger ask_user with options",
      "Verify the prompt in conversation view is styled as a question, not an error"
    ]
  }
]
```

## Non-Goals (Deferred)

- Moving the option selector to the top of the chat screen (keep in input area for now)
- Scrollable option list for very long option arrays
- Keyboard shortcuts for direct option selection (e.g., press 1/2/3)
