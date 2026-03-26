---
status: pending
---

# Option Selector "Other..." Freeform Entry

## Overview

When `ask_user` presents options with `allow_freeform=true`, the option selector should display an extra "Other..." item at the bottom of the list. When the user highlights "Other..." and presses Enter, the option selector is dismissed and the input switches to normal text input mode so the user can type a free-form response. The typed text is then submitted as the interaction response.

Currently `allow_freeform` is stored in `OptionSelectState` but marked `#[allow(dead_code)]` and never used.

## Requirements

### 1. Append "Other..." to options when freeform is allowed (`crates/quine-cli/src/tui/app.rs`)

When building `OptionSelectState` in the `INTERACTION_NEEDED` handler (around line 789), if `allow_freeform` is true, append `"Other..."` to the options list:

```rust
let mut select_options = options.clone();
if allow_freeform {
    select_options.push("Other...".to_string());
}
self.option_select = Some(OptionSelectState {
    options: select_options,
    cursor: 0,
    selected: HashSet::new(),
    multi_select: kind == InteractionKind::MultiSelect,
    allow_freeform,
});
```

### 2. Handle "Other..." selection in `submit_input` (`crates/quine-cli/src/tui/app.rs`)

In `submit_input()` (line 386), when the option selector is active:

```rust
if let Some(select) = self.option_select.take() {
    // Check if "Other..." was selected (last item when allow_freeform)
    if select.allow_freeform && select.cursor == select.options.len() - 1 {
        // Switch to text input mode — keep the interaction in the queue,
        // don't submit yet. The user will type and submit via normal path.
        return None;
    }
    // ... existing selection logic ...
}
```

When "Other..." is selected, `option_select` is already `take()`n (dismissed), and the interaction remains in `interaction_queue`. The next `submit_input` call will follow the normal text input path (line 410+) and submit the typed text as the interaction response.

### 3. Render "Other..." distinctly in UI (`crates/quine-cli/src/tui/ui.rs`)

In the option selector rendering (draw_input), render the "Other..." entry in a dimmed/italic style to visually distinguish it from real options:

```rust
let style = if is_cursor {
    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
} else if select.allow_freeform && i == select.options.len() - 1 {
    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)
} else {
    Style::default()
};
```

### 4. Remove `#[allow(dead_code)]` from `allow_freeform` (`crates/quine-cli/src/tui/app.rs`)

The field is now used — remove the dead_code annotation from both `OptionSelectState` and `PendingInteraction`.

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass
- When `allow_freeform=true`, "Other..." appears as the last option in the selector
- Selecting "Other..." dismisses the selector and allows typing a free-form response
- The typed response is submitted as the interaction answer
- When `allow_freeform=false`, no "Other..." option appears (unchanged behavior)
- Existing tests continue to pass
- New unit tests:
  - `freeform_other_appended` — verify "Other..." is added when allow_freeform=true
  - `freeform_other_dismisses_selector` — verify selecting "Other..." returns None and clears option_select
  - `freeform_other_not_shown_when_disabled` — verify no "Other..." when allow_freeform=false

## QA Test Cases

```json
[
  {
    "name": "option_selector_other_freeform",
    "description": "Selecting Other... switches to text input for free-form response",
    "steps": [
      "Trigger ask_user with options and allow_freeform=true",
      "Verify 'Other...' appears at bottom of option list",
      "Navigate to 'Other...' and press Enter",
      "Verify option selector disappears and input box is active",
      "Type a custom response and press Ctrl+S",
      "Verify the custom text is submitted"
    ]
  }
]
```

## Non-Goals (Deferred)

- Inline text input within the option selector (type to filter)
- "Other..." for MultiSelect (only applies to SingleSelect for now)
- Keyboard shortcut (e.g., 'o') to jump directly to Other
