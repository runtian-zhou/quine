---
status: pending
---

# Trim Leading/Trailing Blank Lines from Assistant Text in TUI

## Overview

The TUI currently renders extra blank lines between the user prompt ("You: ...") and the assistant's response text. This happens because the LLM response text often contains leading newlines/whitespace that get preserved verbatim through the rendering pipeline, producing visible empty lines in the conversation view.

Example of current (broken) rendering:
```
You: give me a summary of quine-core




  I'll explore the quine-core crate to give you a comprehensive summary.
```

Expected rendering:
```
You: give me a summary of quine-core

  I'll explore the quine-core crate to give you a comprehensive summary.
```

## Root Cause

The assistant text flows through this path:
1. **Accumulation** (`crates/quine-cli/src/tui/app.rs`): streaming deltas are appended to `streaming_buffer`, then flushed as `ConversationEntry::AssistantText(text)` on `TEXT_COMPLETE` or `TOOL_REQUEST` notifications.
2. **Rendering** (`crates/quine-cli/src/tui/ui.rs`, lines 50-53): `text.lines()` iterates over every line including empty leading/trailing ones, and each becomes a rendered `Line`.

Neither step trims leading or trailing blank lines from the text content.

## Requirements

### 1. Trim blank lines when storing assistant text (`crates/quine-cli/src/tui/app.rs`)

At each point where `ConversationEntry::AssistantText(text)` is pushed to `self.messages`, trim leading and trailing blank lines (not internal blank lines — those are intentional paragraph breaks):

- **`TEXT_COMPLETE` handler** (~line 562-563): trim the text before pushing.
- **`TOOL_REQUEST` handler** (~line 571-572): trim the text before pushing.
- **`TURN_COMPLETE` handler** (~line 680-681): trim the text before pushing.

Implement a helper function to strip leading/trailing blank lines:

```rust
/// Strip leading and trailing blank lines from text while preserving internal blank lines.
fn trim_blank_lines(text: &str) -> &str {
    let trimmed = text.trim_matches('\n');
    // Also handle \r\n line endings
    trimmed.trim_matches('\r')
}
```

Or more robustly, trim lines that are empty or whitespace-only from the start and end:

```rust
fn trim_blank_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
    let end = lines.iter().rposition(|l| !l.trim().is_empty()).map(|i| i + 1).unwrap_or(0);
    lines[start..end].join("\n")
}
```

### 2. Add a single blank line separator between conversation entries (`crates/quine-cli/src/tui/ui.rs`)

To ensure clean visual separation between entries without relying on the text content having blank lines, add an empty `Line` between each conversation entry in `draw_conversation`. Insert a blank separator line before each entry (except the first):

```rust
for (i, entry) in app.messages.iter().enumerate() {
    if i > 0 {
        lines.push(Line::from(""));
    }
    match entry {
        // ... existing match arms ...
    }
}
```

This gives consistent 1-line spacing between all entries regardless of text content.

### 3. Trim streaming buffer display (`crates/quine-cli/src/tui/ui.rs`, ~line 170-177)

Apply the same leading blank line trimming when rendering the live streaming buffer so that streamed text also doesn't show leading blank lines.

## Acceptance Criteria

- `cargo build` / `cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --all -- --check` must pass.
- No extra blank lines appear between "You: ..." and the first line of assistant text.
- Internal blank lines within assistant text (paragraph breaks) are preserved.
- Consistent single blank line spacing between all conversation entries.
- Existing unit tests in `app.rs` continue to pass (especially `apply_text_complete_flushes_buffer` and `apply_turn_complete_flushes_buffer`).
- New unit tests:
  - `trim_blank_lines` correctly strips leading/trailing blank lines while preserving internal ones.
  - `AssistantText` entries with leading newlines are stored trimmed.

## QA Test Cases (add to `qa/test_cases.json`)

```json
[
  {
    "name": "no_leading_blank_lines_in_assistant_text",
    "description": "Assistant text should not have leading blank lines after user prompt",
    "input": "say hello",
    "expect": {
      "type": "output_pattern",
      "pattern": "You:.*\\n\\n  \\S",
      "description": "After 'You: ...' there should be at most one blank line before indented assistant text starts"
    }
  }
]
```

## Non-Goals (Deferred)

- Full markdown rendering (bold, italic, headers, code blocks) — that's a separate feature.
- Configurable spacing between entries.
- Trimming of internal excessive blank lines (e.g., 3+ consecutive blank lines within text).
