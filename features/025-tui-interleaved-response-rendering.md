---
status: done
---

# TUI Interleaved Response and Tool Result Rendering

## Overview

Currently, when the LLM emits text followed by tool calls in a single response, the assistant text always appears at the end of the conversation — after all tool results. This happens because:

1. **`text_before` is never emitted to the TUI**: In `engine.rs:609`, when the LLM returns `ToolCalls { text_before, calls }`, the `text_before` text is recorded in conversation history but no `TextComplete` event is sent to the UI. The streaming deltas _are_ sent, but the `streaming_buffer` in the TUI is never flushed before tool execution begins.

2. **The streaming buffer accumulates across tool boundaries**: In `tui/app.rs:529`, `streaming_buffer` keeps accumulating text. When the engine starts executing tools (emitting `ToolRequest`), the TUI transitions to `RunningTool` phase but the buffered text remains unflushed — it only gets flushed on `TextComplete` (which only fires when the final LLM call has no tool calls) or `TurnComplete`.

The result is that a turn like "I'll read the file → [read tool] → Here are the contents" renders as:

```
[ToolCall: read ✓ 45ms]
I'll read the file
Here are the contents
```

Instead of the expected interleaved order:

```
I'll read the file
[ToolCall: read ✓ 45ms]
Here are the contents
```

## Requirements

### 1. Emit `TextComplete` before tool execution (`crates/quine-core/src/engine.rs`)

In `handle_llm_turn()`, when the LLM returns `LlmTurnResult::ToolCalls { text_before, calls }` (around line 609), emit a `TextComplete` event for `text_before` **before** emitting any `ToolRequest` events:

```rust
LlmTurnResult::ToolCalls { text_before, calls } => {
    // NEW: Flush any text that preceded the tool calls to the TUI.
    if let Some(ref text) = text_before {
        let _ = output
            .send(CoreOutput::TextComplete {
                session_id,
                full_text: text.clone(),
            })
            .await;
    }

    // ... existing tool_use_requests, history push, tool execution loop ...
}
```

This ensures the TUI receives `TextComplete` → `ToolRequest` → `ToolResult` → (next round) `StreamDelta` → `TextComplete` in proper interleaved order.

### 2. Flush streaming buffer on `ToolRequest` (`crates/quine-cli/src/tui/app.rs`)

As a defensive measure, in `apply_notification()` for `notifications::TOOL_REQUEST` (around line 550), flush the `streaming_buffer` into `messages` before adding the `ToolCall` entry:

```rust
notifications::TOOL_REQUEST => {
    // Flush any streaming text that preceded this tool call.
    if !self.streaming_buffer.is_empty() {
        let text = std::mem::take(&mut self.streaming_buffer);
        self.messages.push(ConversationEntry::AssistantText(text));
    }

    // ... existing ToolRequest handling ...
}
```

This guarantees correct ordering even if there's a race between `TextComplete` and `ToolRequest` delivery.

### 3. Include tool output in `ToolResult` notification (optional enhancement)

Currently `CoreOutput::ToolResult` only includes `is_error` and `duration_ms` — the actual tool output is invisible to the TUI. To enable richer rendering (e.g., showing a snippet of bash output), add an optional `output_preview` field:

In `crates/quine-core/src/channel.rs`, extend `CoreOutput::ToolResult`:

```rust
ToolResult {
    session_id: SessionId,
    tool_use_id: String,
    tool_name: String,
    is_error: bool,
    duration_ms: u64,
    output_preview: Option<String>,  // NEW: truncated tool output (max ~200 chars)
}
```

In `crates/quine-core/src/engine.rs`, populate it when emitting (around line 679):

```rust
let preview = if tool_output.len() > 200 {
    Some(format!("{}…", &tool_output[..200]))
} else if tool_output.is_empty() {
    None
} else {
    Some(tool_output.clone())
};

let _ = output
    .send(CoreOutput::ToolResult {
        session_id,
        tool_use_id: call.tool_use_id.clone(),
        tool_name: call.tool_name.clone(),
        is_error,
        duration_ms: tool_duration_ms,
        output_preview: preview,
    })
    .await;
```

Update `crates/quine-harness/src/server.rs` (`core_output_to_notification`) and `crates/quine-cli/src/tui/app.rs` (`TOOL_RESULT` handler) to pass through the new field.

In `crates/quine-cli/src/tui/ui.rs`, optionally render the preview below the tool status line (dimmed, single line).

### 4. Update chat renderer (`crates/quine-cli/src/chat.rs`)

Apply the same streaming-buffer flush logic in the non-TUI chat renderer's `handle_notification()` for `TOOL_REQUEST`, to keep both renderers consistent.

## Acceptance Criteria

- `cargo build` compiles without errors
- `cargo test` passes (all existing + new tests)
- `cargo clippy --all-targets -- -D warnings` has zero warnings
- `cargo fmt --all -- --check` passes
- When the LLM returns text + tool calls, the text appears **before** the tool call entries in the TUI
- Each round of LLM text → tool execution → LLM text renders in chronological order
- The streaming buffer is always flushed before tool call entries are added

### Unit Tests Required

- **`engine.rs`**: Test that `TextComplete` is emitted before `ToolRequest` when `text_before` is `Some`. Add a test case in the existing `mod tests` that uses a mock provider returning `TextDelta` + `ToolCall` + `Done`, and asserts the `CoreOutput` event order is `StreamDelta` → `TextComplete` → `ToolRequest` → `ToolResult`.
- **`tui/app.rs`**: Test that `apply_notification` for `TOOL_REQUEST` flushes `streaming_buffer` into `messages` before the `ToolCall` entry. Create a sequence: send `STREAM_DELTA`, then `TOOL_REQUEST`, and verify `messages[0]` is `AssistantText` and `messages[1]` is `ToolCall`.

## QA Test Cases (add to `qa/test_cases.json`)

```json
[
  {
    "name": "interleaved_text_and_tool_rendering",
    "description": "Verify LLM text appears before tool calls in the TUI conversation",
    "steps": [
      "Start a session with `quine chat`",
      "Send a message that triggers tool use with preceding text (e.g., 'read the file Cargo.toml and summarize it')",
      "Observe the TUI output order"
    ],
    "expected": "Assistant text ('I\\'ll read...') appears BEFORE the tool call entry, and the summary text appears AFTER the tool result"
  },
  {
    "name": "multiple_tool_rounds_interleaved",
    "description": "Verify multiple rounds of text+tools render in chronological order",
    "steps": [
      "Start a session with `quine chat`",
      "Send a message that triggers multiple sequential tool calls (e.g., 'read Cargo.toml and then read README.md')",
      "Observe the TUI output order"
    ],
    "expected": "Each text segment appears between its adjacent tool call entries, maintaining chronological order"
  }
]
```

## Non-Goals (Deferred)

- **Collapsible tool output**: Rendering full tool output inline with expand/collapse is deferred to a separate feature.
- **Parallel tool call rendering**: If the LLM requests multiple tools in one response, they currently render sequentially. Parallel rendering layout is out of scope.
- **Syntax highlighting of tool output**: Preview text is rendered as plain dimmed text for now.
