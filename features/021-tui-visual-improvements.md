---
status: pending
---

# TUI Visual Improvements: Indentation, Timing, Tokens, and Tool Status Colors

## Overview

The TUI currently renders all conversation entries at the same indentation level with minimal visual hierarchy. Tool calls and LLM responses are hard to distinguish at a glance. There is no timing or token usage information, and tool call status (running / succeeded / failed) is not indicated.

This feature improves the TUI with:
1. **Indentation** — tool calls and their results are visually nested under the LLM turn
2. **Timing and token info** — each LLM turn shows elapsed time and token counts
3. **Tool status colors** — tool calls show colored status indicators (running/success/error)

## Requirements

### 1. Track timing and token usage in LlmEvent and CoreOutput

**File**: `crates/quine-llm/src/types.rs`

Add usage info to `LlmEvent::Done`:

```rust
pub enum LlmEvent {
    TextDelta { text: String },
    ToolCall { tool_use_id: String, tool_name: String, arguments: serde_json::Value },
    Done {
        usage: Option<TokenUsage>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}
```

**File**: `crates/quine-llm/src/anthropic.rs`

Parse the `usage` field from Anthropic's `message_stop` or `message_delta` SSE events. Anthropic sends `usage: { input_tokens, output_tokens }` in the `message_delta` event right before `message_stop`. Populate `LlmEvent::Done { usage }` accordingly.

**File**: `crates/quine-llm/src/openai_compat.rs`

Parse `usage` from the OpenAI-compatible stream's final chunk if available. Set `None` if not provided.

### 2. Add TurnComplete with timing and token info in CoreOutput

**File**: `crates/quine-core/src/channel.rs`

Extend `TurnComplete` to carry timing and usage:

```rust
TurnComplete {
    session_id: SessionId,
    duration_ms: u64,
    usage: Option<quine_llm::TokenUsage>,
},
```

**File**: `crates/quine-core/src/engine.rs`

In `handle_llm_turn()`, track the start time with `Instant::now()` before the loop begins. Accumulate `TokenUsage` from each `LlmEvent::Done` across multiple LLM calls in the same turn (tool call loops). When emitting `TurnComplete`, include the elapsed duration and accumulated usage.

**File**: `crates/quine-harness/src/server.rs`

Include `duration_ms` and `usage` (if present) in the `TURN_COMPLETE` notification params:

```json
{
  "session_id": "...",
  "duration_ms": 4523,
  "usage": { "input_tokens": 1200, "output_tokens": 350 }
}
```

### 3. Add ToolResult event to CoreOutput

Currently, the TUI knows when a tool starts (`ToolRequest`) but not when it finishes or whether it succeeded. Add a new event:

**File**: `crates/quine-core/src/channel.rs`

```rust
ToolResult {
    session_id: SessionId,
    tool_use_id: String,
    tool_name: String,
    is_error: bool,
    duration_ms: u64,
},
```

**File**: `crates/quine-core/src/engine.rs`

After each tool execution in the `for call in &calls` loop (~line 555), emit `CoreOutput::ToolResult` with the outcome and elapsed time. Time each tool call individually with `Instant::now()` before `execute_tool_call`.

**File**: `crates/quine-harness/src/server.rs`

Add notification conversion for `ToolResult` → `notifications::TOOL_RESULT`:

```json
{
  "session_id": "...",
  "tool_use_id": "...",
  "tool_name": "bash",
  "is_error": false,
  "duration_ms": 1200
}
```

**File**: `crates/quine-harness/src/protocol.rs`

Add the notification constant:

```rust
pub const TOOL_RESULT: &str = "tool_result";
```

### 4. Update ConversationEntry for richer tool display

**File**: `crates/quine-cli/src/tui/app.rs`

Extend `ConversationEntry::ToolCall` to track status:

```rust
pub enum ToolStatus {
    Running,
    Success { duration_ms: u64 },
    Error { duration_ms: u64 },
}

pub enum ConversationEntry {
    User(String),
    AssistantText(String),
    ToolCall {
        tool_name: String,
        tool_use_id: String,
        summary: String,
        status: ToolStatus,
    },
    WriteDiff { file_path: String, diff_lines: Vec<DiffLine> },
    Error(String),
    InteractionPrompt(String),
    TurnInfo { duration_ms: u64, usage: Option<TokenUsage> },
}
```

Add `TurnInfo` as a separator entry shown after each completed turn.

### 5. Handle new notifications in apply_notification

**File**: `crates/quine-cli/src/tui/app.rs`

In `apply_notification()`:

a. **TOOL_REQUEST** — store the `tool_use_id` in the `ToolCall` entry and set `status: ToolStatus::Running`.

b. **TOOL_RESULT** — find the matching `ToolCall` entry by `tool_use_id` and update its status:

```rust
notifications::TOOL_RESULT => {
    let tool_use_id = params["tool_use_id"].as_str();
    let is_error = params["is_error"].as_bool().unwrap_or(false);
    let duration_ms = params["duration_ms"].as_u64().unwrap_or(0);
    // Find matching ToolCall entry (search from end)
    for entry in self.messages.iter_mut().rev() {
        if let ConversationEntry::ToolCall { tool_use_id: id, status, .. } = entry {
            if id.as_str() == tool_use_id {
                *status = if is_error {
                    ToolStatus::Error { duration_ms }
                } else {
                    ToolStatus::Success { duration_ms }
                };
                break;
            }
        }
    }
}
```

c. **TURN_COMPLETE** — extract `duration_ms` and `usage`, push `TurnInfo`:

```rust
notifications::TURN_COMPLETE => {
    let duration_ms = params["duration_ms"].as_u64().unwrap_or(0);
    let usage = /* parse from params */;
    if duration_ms > 0 {
        self.messages.push(ConversationEntry::TurnInfo { duration_ms, usage });
    }
    self.phase = AgentPhase::Idle;
}
```

### 6. Render with indentation and colors

**File**: `crates/quine-cli/src/tui/ui.rs`

Update `draw_conversation()` rendering:

#### a. Indentation hierarchy

- **User messages**: no indent, green bold `"You: "` prefix (unchanged)
- **AssistantText**: indent 2 spaces, default color (prefix `"  "`)
- **ToolCall**: indent 4 spaces, colored status marker
- **WriteDiff**: indent 4 spaces (unchanged behavior, just indented)
- **TurnInfo**: indent 2 spaces, dim style

#### b. Tool status colors and markers

```rust
ConversationEntry::ToolCall { tool_name, summary, status, .. } => {
    let (marker, style) = match status {
        ToolStatus::Running => ("⟳", Style::default().fg(Color::Yellow)),
        ToolStatus::Success { duration_ms } => ("✓", Style::default().fg(Color::Green)),
        ToolStatus::Error { duration_ms } => ("✗", Style::default().fg(Color::Red)),
    };
    let duration_str = match status {
        ToolStatus::Running => String::new(),
        ToolStatus::Success { duration_ms } | ToolStatus::Error { duration_ms } => {
            format!(" ({:.1}s)", *duration_ms as f64 / 1000.0)
        }
    };
    // Render: "    ✓ bash: echo hello (1.2s)"
    //     or: "    ⟳ bash: echo hello"
    //     or: "    ✗ bash: exit 42 (0.3s)"
    Line::from(vec![
        Span::raw("    "),
        Span::styled(marker, style),
        Span::styled(format!(" {tool_name}: {summary}{duration_str}"), Style::default().add_modifier(Modifier::DIM)),
    ])
}
```

#### c. Turn info line

```rust
ConversationEntry::TurnInfo { duration_ms, usage } => {
    let time_str = format!("{:.1}s", *duration_ms as f64 / 1000.0);
    let token_str = match usage {
        Some(u) => format!(" | {} in / {} out tokens", u.input_tokens, u.output_tokens),
        None => String::new(),
    };
    // Render: "  ── 4.5s | 1200 in / 350 out tokens ──"
    Line::from(vec![
        Span::styled(
            format!("  ── {time_str}{token_str} ──"),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        ),
    ])
}
```

## Acceptance Criteria

- `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check` must pass
- All existing tests continue to pass
- Tool calls render with colored status markers (yellow=running, green=success, red=error)
- Tool calls and LLM text are visually indented under the conversation turn
- Each completed turn shows elapsed time
- Token usage is displayed when available (Anthropic provider)
- `--json` oneshot mode includes `duration_ms` and `usage` in output

### Unit Tests

- `tool_status_transitions` (in `tui/app.rs`): Apply `TOOL_REQUEST` then `TOOL_RESULT` notifications; verify `ToolCall` entry status transitions from `Running` to `Success`/`Error`.
- `tool_result_error_status` (in `tui/app.rs`): Apply a `TOOL_RESULT` with `is_error: true`; verify status is `ToolStatus::Error`.
- `turn_info_created_on_complete` (in `tui/app.rs`): Apply `TURN_COMPLETE` with `duration_ms` and `usage`; verify `TurnInfo` entry is pushed.
- `token_usage_serde_roundtrip` (in `quine-llm/src/types.rs`): Verify `TokenUsage` serialization roundtrip.
- `draw_does_not_panic_with_tool_status` (in `tui/ui.rs`): Render with `ToolCall` entries in all three statuses.

## QA Test Cases (add to `.claude/qa-tests.md`)

```markdown
## tui_tool_status_display
**Description**: Verify tool calls show status indicators in TUI output.
- **Flags**: `--json`
- **Send**: `"Use the bash tool to run: echo TOOL_STATUS_TEST"`
- **Expect**: JSON `tool_calls` array is non-empty; response contains `TOOL_STATUS_TEST`
- **Note**: Visual verification of colors requires manual TUI testing.

## tui_turn_timing
**Description**: Verify turn timing is reported.
- **Flags**: `--json`
- **Send**: `"Say hello"`
- **Expect**: JSON output contains `duration_ms` field with value > 0
```

## Non-Goals (Deferred)

- **Cost estimation**: No dollar-cost calculations from token counts.
- **Per-tool-call token breakdown**: Only aggregate per-turn tokens; not per individual LLM call within a turn.
- **Theme/color customization**: Colors are hardcoded; a theme system is out of scope.
- **Streaming token counter**: Real-time token count during streaming; only show final count after turn completes.
