---
status: done
---

# Fix Ask-User Multi-Line Prompt Rendering in TUI

## Overview

When the `ask_user` tool triggers a `SingleSelect` or `MultiSelect` interaction, the prompt and numbered options list are pushed to the conversation view as `ConversationEntry::Error(label)` where `label` contains embedded newlines (e.g., `"❓ Pick a color\n  1. red\n  2. green\n  3. blue"`). However, the `Error` variant renderer in `crates/quine-cli/src/tui/ui.rs:112-117` creates a **single** `Line`:

```rust
ConversationEntry::Error(text) => {
    lines.push(Line::from(Span::styled(
        format!("Error: {text}"),
        Style::default().fg(Color::Red),
    )));
}
```

Ratatui's `Line` does not render embedded `\n` characters — they are either swallowed or displayed as a single glyph. The result is that the question and options are invisible or collapsed in the conversation history. The user only sees the select widget in the input box but has no context in the chat above.

This is a residual bug from feature 022 (`fix-ask-user-options-rendering`), which fixed the data flow but left the rendering broken.

## Requirements

### 1. Add a dedicated `ConversationEntry` variant (`crates/quine-cli/src/tui/app.rs`)

Replace the use of `ConversationEntry::Error` for interaction prompts. Add a new variant to the `ConversationEntry` enum (around line 31):

```rust
pub enum ConversationEntry {
    // ... existing variants ...
    /// An interaction prompt from the agent (ask_user, permission, etc.)
    /// Contains the prompt text and optional numbered options.
    InteractionQuestion {
        prompt: String,
        options: Vec<String>,
        source_label: Option<String>,
    },
}
```

### 2. Use the new variant when handling `INTERACTION_NEEDED` (`crates/quine-cli/src/tui/app.rs`)

In `apply_notification()` for `notifications::INTERACTION_NEEDED` (around line 694), replace the current block that builds a `label` string and pushes `ConversationEntry::Error(label)` (lines 746–760) with:

```rust
// For permission prompts, keep using a single-line format.
if kind == InteractionKind::Permission {
    let source_prefix = source_label
        .as_deref()
        .map(|s| format!("[{s}] "))
        .unwrap_or_default();
    self.messages.push(ConversationEntry::InteractionQuestion {
        prompt: format!("{source_prefix}⚠ Permission: {prompt}"),
        options: Vec::new(),
        source_label: None,
    });
} else {
    self.messages.push(ConversationEntry::InteractionQuestion {
        prompt: prompt.clone(),
        options: options.clone(),
        source_label: source_label.clone(),
    });
}
```

### 3. Render the new variant in `ui.rs` (`crates/quine-cli/src/tui/ui.rs`)

Add a match arm for `InteractionQuestion` in `draw_conversation()` (after the `Error` arm, around line 117):

```rust
ConversationEntry::InteractionQuestion {
    prompt,
    options,
    source_label,
} => {
    let source_prefix = source_label
        .as_deref()
        .map(|s| format!("[{s}] "))
        .unwrap_or_default();
    // Render the question line
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {source_prefix}❓ "),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            prompt.to_string(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    // Render each option as a separate Line
    for (i, opt) in options.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            format!("     {}. {opt}", i + 1),
            Style::default().fg(Color::Cyan),
        )));
    }
}
```

This ensures:
- The question is styled in yellow/bold (not red error style)
- Each option is a separate `Line` so ratatui renders them on individual rows
- Source labels (e.g., `[subagent: ...]`) are shown when present

### 4. Handle multi-line prompts in `Error` variant as a safety net (`crates/quine-cli/src/tui/ui.rs`)

Since `ConversationEntry::Error` may still be used elsewhere, fix it to split on newlines:

```rust
ConversationEntry::Error(text) => {
    for (i, line) in text.lines().enumerate() {
        let prefix = if i == 0 { "Error: " } else { "       " };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{line}"),
            Style::default().fg(Color::Red),
        )));
    }
}
```

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass
- When `ask_user` triggers with options, the question and each option appear as separate lines in the conversation history
- The prompt renders in yellow (not red error style)
- Options render in cyan with numbered prefixes
- Free-form `ask_user` (no options) still renders correctly as a single question line
- Permission prompts still render correctly
- Source labels from subagents are displayed
- Existing tests continue to pass

### Unit Tests Required

- **`app.rs`**: Test that `apply_notification` for `INTERACTION_NEEDED` with `SingleSelect` kind and options pushes `ConversationEntry::InteractionQuestion` (not `Error`) with the correct `prompt` and `options` fields.
- **`ui.rs`**: Test that rendering an `InteractionQuestion` with 3 options produces 4 lines (1 prompt + 3 options) and does not panic. Follow the existing pattern in `ui.rs::tests`.

## QA Test Cases (add to `qa/test_cases.json`)

```json
[
  {
    "name": "ask_user_options_visible_in_chat",
    "description": "Verify ask_user options render as separate lines in conversation history",
    "steps": [
      "Start TUI chat session",
      "Send a message that triggers ask_user with options (e.g., 'ask me to pick a color with options red, green, blue')",
      "Look at the conversation history area (above the input box)"
    ],
    "expected": "The question appears on one line in yellow, followed by numbered options (1. red, 2. green, 3. blue) on separate lines in cyan"
  },
  {
    "name": "ask_user_freeform_renders_in_chat",
    "description": "Verify ask_user without options still renders the question",
    "steps": [
      "Start TUI chat session",
      "Send a message that triggers ask_user without options (e.g., 'ask me what my name is')",
      "Look at the conversation history"
    ],
    "expected": "The question appears in yellow with ❓ prefix, no option list below it"
  }
]
```

## Non-Goals (Deferred)

- Rendering the user's selected answer back into the conversation as a styled response (already handled by `InteractionPrompt` variant)
- Scrollable option list for very large option arrays
- Keyboard number shortcuts (press 1/2/3) to select options directly
