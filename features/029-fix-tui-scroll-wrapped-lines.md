---
status: done
---

# Fix TUI Scroll for Wrapped Lines

## Overview

When the conversation exceeds one screen page, old messages become inaccessible because the scroll calculation does not account for word-wrapped lines. The `content_height` is computed as `text.lines.len()` (the number of `Line` objects), but with `Wrap { trim: false }` enabled, a single `Line` can occupy multiple screen rows. This causes the scroll math to underestimate total content height, so earlier messages are effectively flushed off the top of the viewport and cannot be scrolled to.

## Root Cause

In `crates/quine-cli/src/tui/ui.rs` (around line 203):

```rust
let content_height = text.lines.len() as u16;
```

This counts logical lines, not visual (wrapped) rows. When a line wraps across 3 screen rows, it still counts as 1 toward `content_height`. The `max_scroll` value is therefore too small, and the viewport cannot reach the top of the conversation.

## Requirements

### 1. Compute Wrapped Line Height (`crates/quine-cli/src/tui/ui.rs`)

Replace the naive `text.lines.len()` calculation with one that accounts for wrapping. For each `Line`, compute the number of visual rows it occupies:

```rust
fn wrapped_line_count(line: &Line, area_width: u16) -> u16 {
    if area_width == 0 {
        return 1;
    }
    let width = line.width() as u16;
    if width == 0 {
        return 1;
    }
    (width + area_width - 1) / area_width // ceiling division
}
```

Then compute total content height as:

```rust
let content_height: u16 = text.lines.iter()
    .map(|line| wrapped_line_count(line, area.width))
    .sum();
```

**Note:** This is an approximation. Ratatui's `Wrap` widget uses a more sophisticated line-breaking algorithm (breaking at word boundaries). For a precise count, consider using `ratatui::widgets::Paragraph::line_count(area_width)` if available in ratatui 0.29, which returns the exact wrapped line count. Check the ratatui API docs — if `line_count` exists, prefer it over manual calculation.

### 2. Update Scroll Calculation (`crates/quine-cli/src/tui/ui.rs`)

The existing scroll logic should work correctly once `content_height` is accurate:

```rust
let max_scroll = content_height.saturating_sub(view_height);
let scroll = if app.user_scrolled {
    max_scroll.saturating_sub(app.scroll_offset.min(max_scroll))
} else {
    max_scroll
};
```

No changes needed here beyond fixing `content_height`.

### 3. Handle u16 Overflow for Long Sessions

For very long conversations, the wrapped line count may exceed `u16::MAX` (65,535). Either:
- Use `u32` for internal height tracking and clamp to `u16` only when passing to ratatui's `scroll()`, or
- Cap `content_height` at `u16::MAX` (acceptable since ratatui's scroll API takes `u16`)

### 4. Scroll Step Size

The current Page Up/Down step of 10 lines is reasonable for unwrapped content but may feel slow with many wrapped lines. Consider making the step proportional to the viewport height (e.g., `view_height.saturating_sub(2)`) so one Page Up/Down moves roughly one screen. This is in `crates/quine-cli/src/tui/mod.rs` (around lines 231-246):

```rust
KeyCode::PageUp => app.scroll_up(view_height.saturating_sub(2)),
KeyCode::PageDown => app.scroll_down(view_height.saturating_sub(2)),
```

This requires passing the current viewport height to the key handler, or storing it in `App` state during rendering.

## Acceptance Criteria

- `cargo build` / `cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --all -- --check` must pass
- User can scroll to the very first message in a conversation that spans multiple screens
- Auto-scroll to bottom still works when new messages arrive (existing behavior preserved)
- Page Up from the top of the conversation does not panic or wrap around
- Home key reaches the absolute top of all content including wrapped lines

### Unit Tests

- **`test_wrapped_line_count_single_row`**: A short line (width < area width) returns 1 row
- **`test_wrapped_line_count_multi_row`**: A line of width 200 in a 80-column area returns 3 rows
- **`test_wrapped_line_count_exact_fit`**: A line of width 80 in 80-column area returns 1 row
- **`test_wrapped_line_count_empty`**: An empty line returns 1 row
- **`test_content_height_with_wrapping`**: Total height for a mix of short and long lines matches expected wrapped row count

## QA Test Cases (add to `qa/test_cases.json`)

```json
[
  {
    "id": "scroll-wrapped-content",
    "description": "User can scroll back to the first message after a long conversation",
    "steps": [
      "Start a chat session",
      "Send a prompt that produces a long multi-paragraph response",
      "Press Home to scroll to the top",
      "Verify the first user message is visible"
    ],
    "expected": "The initial user message and the beginning of the conversation are visible at the top"
  },
  {
    "id": "scroll-auto-follows-new-messages",
    "description": "Auto-scroll follows new content when user has not manually scrolled",
    "steps": [
      "Start a chat session",
      "Send a prompt that produces a long response",
      "Without pressing any scroll keys, send another prompt",
      "Verify the latest response is visible at the bottom"
    ],
    "expected": "The viewport shows the latest message at the bottom without manual scrolling"
  }
]
```

## Non-Goals (Deferred)

- Virtual scrolling / viewport culling for performance (only needed if sessions grow extremely long)
- Mouse wheel scroll support
- Scroll position indicator / scrollbar widget
- Message-level jump (scroll to specific message by index)
