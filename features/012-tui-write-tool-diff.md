---
status: done
---

# TUI: Render Diff on Write Tool Invocation

When the write tool is invoked, show a diff of the changes in the TUI conversation view instead of just the collapsed tool call line.

## Requirements

- When a `tool_request` notification arrives for the `write` tool, extract the `file_path` and `content` arguments.
- Read the existing file content (if any) from the session filesystem via the `read` tool or bash `cat`.
- Compute a unified diff between old and new content.
- Render the diff in the conversation view with syntax coloring:
  - `+` lines in green
  - `-` lines in red
  - `@@` hunk headers in cyan
  - File header in bold
- The diff replaces the collapsed `▶ write: /path/to/file` line for write tool calls.
- If the file is new (doesn't exist yet), show the entire content as `+` lines with a header like `new file: /path/to/file`.

## Implementation

- Add a `similar` crate dependency for diff computation (or use a simple line-by-line diff).
- In `app.rs`, add a new `ConversationEntry::WriteDiff` variant holding the file path and diff lines.
- When processing `tool_request` for `write`, compute the diff and push `WriteDiff` instead of `ToolCall`.
- In `ui.rs`, render `WriteDiff` entries with colored `+`/`-` lines.
- The old file content can be passed from the daemon by including it in the tool request notification, or the CLI can request it. Simplest approach: the core includes the old content (if any) in the `tool_request` arguments or a new field.

## Acceptance Criteria

- Write tool calls show a colored diff in the TUI.
- New files show all lines as additions.
- Existing collapsed tool call rendering unchanged for non-write tools.
- All CI checks pass.
