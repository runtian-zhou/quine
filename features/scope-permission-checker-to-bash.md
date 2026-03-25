---
status: pending
---

# Scope Permission Checker to Bash Tool Only

## Overview

The permission checker currently runs on every tool invocation, which is unnecessary overhead for safe tools like `read`, `write`, `ask_user`, `plan`, etc. Restrict permission checking to only the `bash` tool, where command execution poses actual risk.

## Requirements

### 1. Engine Change (`quine-core/src/engine.rs`)

In the tool execution path (`execute_tool_call` or equivalent), only invoke the `PermissionChecker` when `tool_name == "bash"`. Skip the check entirely for all other tools.

This is a one-line conditional change — no trait modifications needed.

### 2. No Changes To

- `PermissionChecker` trait — stays generic for future extensibility
- `RuleBasedChecker` / `LlmChecker` / `CompositeChecker` — unchanged
- Tool trait — no changes

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass.
- Bash tool invocations still go through permission checking (dangerous commands blocked/confirmed).
- Non-bash tools (read, write, plan, ask_user, spawn, etc.) execute immediately without permission prompts.
- Existing permission checker tests continue to pass.

## QA Test Cases (add to `qa/test_cases.json`)

```json
{
  "name": "read_tool_no_permission_prompt",
  "description": "Verify read tool executes without permission prompt",
  "turns": [
    {
      "message": "Use the read tool to read the file CLAUDE.md. Include the first line in your response.",
      "expect_contains": "CLAUDE.md"
    }
  ]
}
```
