---
status: pending
---

# TUI Multi-line Input Support

## Overview

The TUI input box currently treats all input as single-line. Pressing Enter immediately submits the message, with no way to insert newline characters. The cursor positioning logic (`ui.rs` line 144) hardcodes `cursor_y = area.y + 1`, so even if text wraps visually, the cursor stays pinned to the first row. This makes composing multi-line prompts (code snippets, structured requests) impossible in the TUI.

## Requirements

### 1. Multi-line Input Buffer (`crates/quine-cli/src/tui/app.rs`)

Replace the single `input: String` + `cursor_pos: usize` model with a representation that tracks logical lines:

```rust
pub struct InputBuffer {
    /// Lines of text (each line excludes its trailing '\n')
    lines: Vec<String>,
    /// Cursor row (0-indexed line number)
    row: usize,
    /// Cursor column (byte offset within the current line, UTF-8 aware)
    col: usize,
}
```

The `InputBuffer` must support:
- `insert_char(c)` — insert at cursor, advancing column
- `insert_newline()` — split the current line at `col`, creating a new line below
- `delete_char_before()` — backspace; if at column 0, join with previous line
- `cursor_left/right()` — move within and across line boundaries
- `cursor_up/down()` — move between lines, clamping column to line length
- `to_string()` — join all lines with `'\n'` for submission
- `is_empty()` — true when all lines are empty (or single empty line)

Keep all operations UTF-8 safe using `char_indices()`, matching existing conventions in `app.rs` lines 326-365.

### 2. Key Binding Changes (`crates/quine-cli/src/tui/mod.rs`)

| Key Combo | Current Behavior | New Behavior |
|-----------|-----------------|--------------|
| `Enter` | Submit input | Insert newline |
| `Ctrl+Enter` or `Ctrl+S` | N/A | Submit input |
| `Up` / `Down` (when input is multi-line) | History nav | Cursor up/down within buffer |
| `Up` / `Down` (when input is single-line) | History nav | History nav (unchanged) |

Detection logic: if `input_buffer.lines.len() > 1`, Up/Down navigate the buffer; otherwise they navigate history. When the cursor is on the first line and Up is pressed, or on the last line and Down is pressed in a multi-line buffer, fall through to history navigation.

**Note:** `crossterm::event::KeyCode::Enter` with `KeyModifiers::CONTROL` gives the Ctrl+Enter combo. Verify crossterm 0.28 emits this on macOS/Linux terminals. If Ctrl+Enter is unreliable across terminals, use Ctrl+S as the primary submit binding.

### 3. Cursor Rendering (`crates/quine-cli/src/tui/ui.rs`)

The `draw_input` function must compute the correct `(x, y)` for the cursor across wrapped and multi-line text:

1. Build the display string: `format!("{}{}", label, input_buffer.to_display_string())` where `to_display_string()` joins lines with `'\n'`.
2. Calculate the cursor's display row by summing:
   - For each logical line before the cursor row: `ceil(line_display_len / available_width)` (accounting for the label prefix on line 0).
   - For the cursor's own line: `cursor_col / available_width`.
3. Calculate cursor's display column: `cursor_col % available_width` (plus label offset on the first line).
4. Clamp to the visible area and call `frame.set_cursor_position(...)`.

### 4. Dynamic Input Box Height (`crates/quine-cli/src/tui/ui.rs`)

Currently the input area uses `Constraint::Length(3)` (line 15). Change this to grow with content:

- Minimum height: 3 rows (1 border top + 1 content + 1 border bottom)
- Maximum height: half the terminal height or 12 rows, whichever is smaller
- Height = `min(max_height, content_display_rows + 2)` where `content_display_rows` accounts for wrapping

This requires the layout constraints in `draw_ui` to be recomputed based on current input content.

### 5. Integration with Existing Features

- **Input history** (`input_history: Vec<String>`): Store multi-line strings as-is in history. Restoring from history populates the `InputBuffer` by splitting on `'\n'`.
- **Submit logic** (`submit_input` in `app.rs`): Call `input_buffer.to_string().trim()` and proceed as before.
- **Permission/ask-user prompts**: These are single-line responses. When `pending_interaction` is `Some(...)`, revert to single-line mode (Enter submits, no newline insertion).

## Acceptance Criteria

- `cargo build` / `cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --all -- --check` must all pass
- Existing tests in `app.rs` (`insert_and_delete_chars`, `history_navigation`, `submit_input_*`) must continue to pass (adapted for `InputBuffer`)
- New unit tests required:
  - `input_buffer_insert_newline` — inserting newline splits line correctly
  - `input_buffer_backspace_at_line_start` — joins with previous line
  - `input_buffer_cursor_up_down` — vertical navigation clamping
  - `input_buffer_to_string` — round-trip with newlines
  - `input_buffer_history_restore` — multi-line string restored correctly
  - `multiline_enter_inserts_newline` — Enter key inserts newline in normal mode
  - `ctrl_enter_submits` — Ctrl+Enter submits the input
  - `single_line_up_down_is_history` — Up/Down navigates history when single-line
  - `multi_line_up_down_is_cursor` — Up/Down moves cursor when multi-line

## QA Test Cases (add to `qa/test_cases.json`)

```json
[
  {
    "name": "tui_multiline_input_basic",
    "description": "User can type multiple lines and submit them",
    "steps": [
      "Launch TUI with `cargo run --bin quine -- chat`",
      "Type 'line one'",
      "Press Enter to insert a newline",
      "Type 'line two'",
      "Press Ctrl+Enter (or Ctrl+S) to submit",
      "Verify the submitted message contains both lines separated by a newline"
    ]
  },
  {
    "name": "tui_multiline_cursor_navigation",
    "description": "Cursor moves correctly across lines",
    "steps": [
      "Type 'hello' then Enter then 'world'",
      "Press Up arrow — cursor moves to first line",
      "Press Down arrow — cursor moves back to second line",
      "Verify cursor column clamps when moving to a shorter line"
    ]
  },
  {
    "name": "tui_multiline_backspace_join",
    "description": "Backspace at start of line joins with previous line",
    "steps": [
      "Type 'hello' then Enter then 'world'",
      "Move cursor to start of 'world' (Home or repeated Left)",
      "Press Backspace",
      "Verify lines are joined into 'helloworld' on a single line"
    ]
  },
  {
    "name": "tui_single_line_history_preserved",
    "description": "Up/Down still navigates history for single-line input",
    "steps": [
      "Submit a message 'first message'",
      "Type some text (single line)",
      "Press Up — previous input from history appears",
      "Press Down — returns to current input"
    ]
  },
  {
    "name": "tui_permission_prompt_single_line",
    "description": "Permission prompts remain single-line (Enter submits)",
    "steps": [
      "Trigger a tool that requires permission",
      "At the permission prompt, press Enter",
      "Verify it submits immediately without inserting a newline"
    ]
  }
]
```

## Non-Goals (Deferred)

- Syntax highlighting in the input box
- Mouse click-to-position cursor
- Horizontal scrolling for very long lines (wrap is sufficient for now)
- Copy/paste of multi-line text from system clipboard (relies on terminal passthrough, not our concern)
- Vim/Emacs keybinding modes
