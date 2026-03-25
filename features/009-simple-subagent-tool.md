---
status: pending
---

# Simple Subagent Tool

## Overview

Add a `subagent` tool that spawns a child agent session with a task, waits for it to complete, and returns its output directly to the caller — all in a single tool call. This is the **primary mechanism for delegating subtasks** and should be preferred over the lower-level process control primitives (spawn + wait_child) defined in `agent-process-control-and-ipc.md`.

The subagent tool is the simple, synchronous abstraction. The process control APIs (spawn, wait, signal, IPC) exist for advanced use cases that require concurrency, signaling, or inter-agent communication.

## Relationship to agent-process-control-and-ipc

| Use case | Mechanism |
|----------|-----------|
| Delegate a subtask and get the result | **`subagent` tool** (this feature) — preferred |
| Run multiple tasks concurrently | `spawn` + `wait_child` (process control) |
| Send signals (stop/kill/pause) to a running agent | `signal` (process control) |
| Stream messages between agents | `send_message` / `recv_message` (IPC) |
| Share state between agents | SharedMemory (IPC) |

The system prompt should instruct the LLM to use `subagent` by default for subtask delegation, and only fall back to the process control APIs when concurrency, signaling, or inter-agent communication is needed.

## Requirements

### 1. SubagentTool (`quine-core/src/tool/subagent.rs`)

- **Name**: `subagent`
- **Parameters**:
  ```json
  {
    "task": "string (required) — the task for the child agent",
    "system_prompt": "string (optional) — override the child's system prompt",
    "inherit_history": "boolean (default false) — copy parent conversation history",
    "inherit_filesystem": "boolean (default true) — share parent's filesystem"
  }
  ```
- **Behavior**:
  1. Creates a new child session (using the same mechanism as `SpawnSession`)
  2. Sends the `task` as the initial user message to the child
  3. The child runs its agent loop autonomously (LLM calls + tool execution)
  4. When the child's agent loop completes (LLM returns text without tool calls), collects the final text output
  5. Returns the child's output as the tool result to the parent
  6. The child session is destroyed after completion
- **Blocking**: The tool call blocks until the child completes — the parent's LLM turn is paused while the subagent runs. This is intentional: the parent gets the result in the same turn, just like calling a function.
- **Error handling**: If the child fails (LLM error, tool error, timeout), return `ToolOutput { content: error_description, is_error: true }`
- **Timeout**: Configurable max duration for the child (default: 5 minutes). If exceeded, the child is killed and an error is returned.

### 2. Implementation Approach

The `SubagentTool` needs to:
1. Create a child `SessionContext` (reuse `SessionContext::new` logic)
2. Run the child's agent loop inline (call `handle_llm_turn` in a loop within the tool's `execute()`)
3. Collect the final `TextComplete` output
4. Clean up the child session

This can be implemented by:
- Adding the `LlmProvider` (as `Arc<dyn LlmProvider>`) and `PermissionChecker` to `ExecutionContext` so the subagent tool can call the LLM directly
- Alternatively, using a `core_input` sender to send `SpawnSession` + `WaitSession` messages and await the result via oneshot — this reuses the engine's existing spawn/wait machinery

The second approach (reuse engine machinery) is preferred as it avoids duplicating the agent loop logic.

### 3. Engine Support

- Ensure `SpawnSession` + automatic `WaitSession` can be composed into a single blocking operation from a tool's perspective
- Add a helper: `spawn_and_wait(parent_id, task, inheritance, timeout) -> ExitStatus` that wraps the two-step flow
- The subagent tool calls this helper via the `core_input` channel

### 4. System Prompt Guidance

Update the agent's system prompt (in `SessionContext` or harness config) to include:

```
When you need to delegate a subtask to another agent, use the `subagent` tool.
This spawns a child agent that executes the task and returns its result directly.

Use `subagent` for:
- Research tasks ("read these files and summarize")
- Independent implementation tasks ("implement this function")
- Exploration ("find all files matching this pattern")

Only use the lower-level process control tools (spawn, wait_child, signal) when you need:
- Multiple subtasks running concurrently
- The ability to cancel or pause a running agent
- Inter-agent message passing or shared state
```

### 5. Register in Tool Registry

Add `SubagentTool` to the default tool set in `SessionContext` creation, alongside Read, Write, Bash, AskUser, Plan.

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass.
- Unit tests for `SubagentTool`:
  - Spawn subagent with simple task, verify output returned
  - Subagent with tool usage (e.g., bash echo), verify tool output in result
  - Subagent timeout, verify error returned
  - Subagent LLM failure, verify error returned
- Existing tests continue to pass.

## QA Test Cases (add to `qa/test_cases.json`)

```json
{
  "name": "subagent_simple_task",
  "description": "Subagent executes a simple task and returns result to parent",
  "turns": [
    {
      "message": "Use the subagent tool to delegate this task: 'Say exactly: SUBAGENT_RESULT_777'. Include the subagent's output in your response.",
      "expect_contains": "SUBAGENT_RESULT_777"
    }
  ]
}
```

```json
{
  "name": "subagent_with_tool_use",
  "description": "Subagent uses tools and returns the result",
  "turns": [
    {
      "message": "Use the subagent tool to delegate: 'Use bash to run echo DELEGATED_123 and report the output'. Include what the subagent found.",
      "expect_contains": "DELEGATED_123"
    }
  ]
}
```

```json
{
  "name": "subagent_preferred_over_spawn",
  "description": "Verify agent uses subagent for simple delegation rather than spawn+wait",
  "turns": [
    {
      "message": "Delegate to a child agent: compute 3 * 7 and tell me the result.",
      "expect_contains": "21"
    }
  ]
}
```

## Non-Goals (Deferred)

- Nested subagents (subagent spawning its own subagent) — allow but don't optimize for it
- Streaming output from subagent to parent during execution
- Subagent resource limits beyond timeout
