---
status: done
---

# Agent Process Control and Inter-Agent Communication

## Overview

Map Linux process control and IPC primitives to agent session lifecycle management in quine-core. This enables the LLM to spawn child agents for subtasks, wait for their completion, control them with signal-like semantics, and communicate between agents via pipes, messages, and shared memory.

> **Note:** For simple subtask delegation, the `subagent` tool (see `features/simple-subagent-tool.md`) is the preferred mechanism. It spawns a child, waits for completion, and returns the result in a single blocking tool call. The process control APIs in this feature are for **advanced use cases** that require concurrency (multiple agents running in parallel), signaling (stop/kill/pause), or inter-agent communication (message passing, shared memory). The system prompt should guide the LLM to prefer `subagent` and only use these primitives when the simpler tool is insufficient.

## Linux API → Agent Mapping

| Linux | Agent Equivalent | Channel Message |
|-------|-----------------|-----------------|
| `fork()+exec()` | Spawn child session with task | `CoreInput::SpawnSession` |
| `clone()` flags | Selective context inheritance | `InheritanceFlags` |
| `wait()`/`waitpid()` | Block until child completes | `CoreInput::WaitSession` |
| `WNOHANG` | Non-blocking completion check | `WaitSession { non_blocking: true }` |
| `kill(SIGTERM)` | Graceful stop after current turn | `CoreInput::Signal { signal: Term }` |
| `kill(SIGKILL)` | Immediate cancellation | `CoreInput::Signal { signal: Kill }` |
| `kill(SIGSTOP)` | Pause session | `CoreInput::Signal { signal: Stop }` |
| `kill(SIGCONT)` | Resume paused session | `CoreInput::Signal { signal: Continue }` |
| `getppid()` | Get parent session ID | `SessionTree::parent_of()` |
| exit status | Child output/result to parent | `ExitStatus` enum |
| `pipe()` | One-way message stream between agents | `AgentPipe` (mpsc channel) |
| `write(fd)`/`read(fd)` | Send/receive messages on a pipe | `SendMessageTool` / `RecvMessageTool` |
| `shmget()`/`shmat()` | Shared key-value store between agents | `SharedMemory` (Arc<RwLock<HashMap>>) |

## Requirements

### 1. Session Types (`quine-core/src/session.rs`)

Add types:
- `InheritanceFlags { history: bool, filesystem: bool, tools: bool, working_directory: bool }` — controls what a child inherits from its parent (default: history=false, filesystem=true, tools=true, working_directory=true)
- `SessionSignal` enum: `Term`, `Kill`, `Stop`, `Continue`
- `ExitStatus` enum: `Success { output: String }`, `Failed { error: String }`, `Killed`, `Cancelled`

### 2. Session Tree (`quine-core/src/session_tree.rs`)

`SessionTree` struct tracking parent-child relationships:
- `add_child(parent, child)` — register a parent-child relationship
- `parent_of(session) -> Option<SessionId>` — get parent (getppid)
- `children_of(session) -> &[SessionId]` — list children
- `record_exit(session, status)` — record exit status and notify waiters
- `register_waiter(child, oneshot::Sender<ExitStatus>)` — register for notification when child exits

### 3. Channel Messages (`quine-core/src/channel.rs`)

Add to `CoreInput`:
- `SpawnSession { parent_id, child_id, task, system_prompt, inheritance, reply: oneshot::Sender }` — fork+exec
- `Signal { session_id, signal: SessionSignal }` — kill(sig)
- `WaitSession { parent_id, child_id, reply: oneshot::Sender, non_blocking: bool }` — wait/waitpid
- `SendMessage { from: SessionId, to: SessionId, content: String }` — write(fd)

Add to `CoreOutput`:
- `ChildSpawned { parent_id, child_id }`
- `ChildExited { parent_id, child_id, status: ExitStatus }`
- `MessageReceived { session_id, from: SessionId, content: String }`

### 4. Inter-Agent Communication (`quine-core/src/ipc.rs`)

Three IPC mechanisms:

**Pipes** (`AgentPipe`):
- Unidirectional message stream between two sessions, backed by `tokio::sync::mpsc`
- `PipeId(Uuid)` identifies each pipe
- `PipeRegistry` — tracks open pipes per session, creates pipe pairs
- `create_pipe() -> (PipeId, SenderEnd, ReceiverEnd)` factory
- Messages are strings suitable for LLM consumption

**Shared Memory** (`SharedMemory`):
- Named key-value store: `Arc<RwLock<HashMap<String, String>>>`
- Created by parent, handle passed to children via `InheritanceFlags`
- Any session with the handle can read/write keys

**Message Mailbox** (`MessageMailbox`):
- Per-session queue of incoming messages from other agents
- `AgentMessage { from: SessionId, content: String, timestamp: DateTime }`
- Backed by `tokio::sync::mpsc`

### 5. Tools

#### `SpawnTool` (`quine-core/src/tool/spawn.rs`)
- **Name**: `spawn`
- **Params**: `task` (string, required), `system_prompt` (string, optional), `inherit_history` (bool, default false), `inherit_filesystem` (bool, default true)
- Sends `CoreInput::SpawnSession`, returns child session ID

#### `WaitChildTool` (`quine-core/src/tool/wait_child.rs`)
- **Name**: `wait_child`
- **Params**: `child_id` (string, required), `non_blocking` (bool, default false)
- Sends `CoreInput::WaitSession`, returns `ExitStatus`

#### `SignalTool` (`quine-core/src/tool/signal.rs`)
- **Name**: `signal`
- **Params**: `session_id` (string, required), `signal` ("term"|"kill"|"stop"|"continue", required)
- Sends `CoreInput::Signal`

#### `SendMessageTool` (`quine-core/src/tool/send_message.rs`)
- **Name**: `send_message`
- **Params**: `target` (string — session_id or pipe_id), `content` (string), `key` (optional, for shared memory)
- If target is a session_id: delivers to target's mailbox
- If target is a pipe: writes to the pipe channel

#### `RecvMessageTool` (`quine-core/src/tool/recv_message.rs`)
- **Name**: `recv_message`
- **Params**: `source` (string — session_id, pipe_id, or "any"), `non_blocking` (bool, default false), `key` (optional, for shared memory)
- Blocking: waits for a message to arrive
- Non-blocking: returns immediately with empty result if no message

### 6. Engine Integration (`quine-core/src/engine.rs`)

- Change `provider` to `Arc<dyn LlmProvider>` for sharing across child tasks
- Add `SessionTree` to loop state
- Add `cancel_token` (CancellationToken from `tokio-util`) and `stop_after_turn` to `SessionContext`
- Add `mailbox: MessageMailbox`, `pipes: PipeRegistry`, `shared_memory: Option<SharedMemory>` to `SessionContext`
- Pass `core_input` sender to `ExecutionContext` so tools can send messages back to the core loop

**`run_child_session()` function** — spawned as a tokio task:
1. Receives task as initial user message
2. Loops calling `handle_llm_turn` until LLM produces text without tool calls, or cancel token fires
3. Checks `stop_after_turn` after each turn for graceful SIGTERM
4. Checks `state == Paused` and awaits resume for SIGSTOP/SIGCONT
5. On completion: records `ExitStatus` in tree, emits `CoreOutput::ChildExited`

**Main loop handlers**:
- `SpawnSession`: build child context from parent + inheritance flags, spawn tokio task
- `Signal`: Term → set `stop_after_turn`; Kill → cancel token; Stop → set Paused; Continue → set Running + notify
- `WaitSession`: return cached exit status or register oneshot waiter
- `SendMessage`: deliver to target session's mailbox

**Child sessions own their `SessionContext`** — moved into the spawned task, no Arc<Mutex> contention. Per-session routing: main loop demultiplexes `InteractionResponse` by session_id to the right child's channel.

### 7. Harness Updates

- `crates/quine-harness/src/server.rs` — handle `ChildSpawned`, `ChildExited`, `MessageReceived` in notification conversion
- `crates/quine-harness/src/protocol.rs` — add notification constants

### 8. Dependencies

- Add `tokio-util` to `quine-core/Cargo.toml` for `CancellationToken`
- Add `chrono` to `quine-core/Cargo.toml` for `AgentMessage` timestamps

## Crate Structure Changes

```
crates/quine-core/src/
  session_tree.rs       # NEW: SessionTree, parent-child tracking
  ipc.rs                # NEW: AgentPipe, SharedMemory, MessageMailbox, PipeRegistry
  tool/
    spawn.rs            # NEW: SpawnTool
    wait_child.rs       # NEW: WaitChildTool
    signal.rs           # NEW: SignalTool
    send_message.rs     # NEW: SendMessageTool
    recv_message.rs     # NEW: RecvMessageTool
  session.rs            # MODIFIED: add InheritanceFlags, SessionSignal, ExitStatus
  channel.rs            # MODIFIED: add SpawnSession, Signal, WaitSession, SendMessage, ChildSpawned, ChildExited, MessageReceived
  engine.rs             # MODIFIED: run_child_session(), new message handlers, SessionContext fields
  tool/mod.rs           # MODIFIED: new modules, core_input sender in ExecutionContext
  lib.rs                # MODIFIED: new module re-exports
```

## Key Design Decisions

- **Child sessions run as spawned tokio tasks** — not blocking the main event loop. Each child owns its `SessionContext` (no Arc<Mutex> contention).
- **Per-session routing** — main loop demultiplexes `InteractionResponse` by session_id to the right child's channel.
- **Shared filesystem** — when `inherit_filesystem: true`, child shares parent's `Arc<dyn SessionFilesystem>`. Writes are visible to both (thread-like). For isolation, set `false` to get a fresh overlay.
- **Graceful vs immediate** — SIGTERM sets a flag checked between LLM turns. SIGKILL cancels the CancellationToken, aborting the task.

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass.
- Unit tests for `SessionTree`: parent-child tracking, exit status recording, waiter notification
- Unit tests for `SpawnTool`: spawn child, verify session created
- Unit tests for `WaitChildTool`: wait for child, collect exit status (blocking + non-blocking)
- Unit tests for `SignalTool`: SIGTERM graceful stop, SIGKILL immediate kill, SIGSTOP/SIGCONT pause/resume
- Unit tests for `SendMessageTool`/`RecvMessageTool`: send message between sessions, blocking/non-blocking recv
- Unit tests for `AgentPipe`: create pipe, write/read, closed pipe behavior
- Unit tests for `SharedMemory`: concurrent read/write from multiple sessions
- Unit tests for `MessageMailbox`: queue messages, drain in order
- Integration test: spawn child → child runs → child exits → parent collects output
- Integration test: parent sends message to child → child receives and acts on it
- Existing tests continue to pass

## QA Test Cases (add to `qa/test_cases.json`)

```json
{
  "name": "spawn_child_and_wait",
  "description": "Spawn a child agent with a task, wait for completion, verify output is collected",
  "turns": [
    {
      "message": "Use the spawn tool to create a child agent with task: 'Say exactly: CHILD_OUTPUT_123'. Then use wait_child to collect its result. Include the child's output in your response.",
      "expect_contains": "CHILD_OUTPUT_123"
    }
  ]
}
```

```json
{
  "name": "signal_term_child",
  "description": "Spawn a child and send SIGTERM to gracefully stop it",
  "turns": [
    {
      "message": "Use spawn to create a child agent with a long task. Then immediately use the signal tool to send 'term' to it. Then use wait_child to confirm it stopped. Report the exit status.",
      "expect_contains": "Killed"
    }
  ]
}
```

```json
{
  "name": "non_blocking_wait",
  "description": "Verify non-blocking wait returns immediately when child is still running",
  "turns": [
    {
      "message": "Spawn a child agent. Immediately call wait_child with non_blocking=true. Report whether you got a result or null.",
      "expect_contains": "null"
    }
  ]
}
```

```json
{
  "name": "parent_child_messaging",
  "description": "Parent sends a message to child, child receives and includes it in output",
  "turns": [
    {
      "message": "Spawn a child agent with task: 'Use recv_message to read a message from your parent, then say the message content'. Then use send_message to send 'SECRET_MSG_789' to the child. Wait for the child and include its output.",
      "expect_contains": "SECRET_MSG_789"
    }
  ]
}
```

```json
{
  "name": "shared_memory_coordination",
  "description": "Parent and child share state via shared memory",
  "turns": [
    {
      "message": "Spawn a child with inherit_filesystem=true and task: 'Use send_message with key=result to write SHARED_42 to shared memory, then complete'. Wait for the child. Then use recv_message with key=result to read from shared memory. Include the value.",
      "expect_contains": "SHARED_42"
    }
  ]
}
```

```json
{
  "name": "child_permission_check",
  "description": "Verify permission checker applies to child agent's bash tool usage",
  "turns": [
    {
      "message": "Spawn a child with task: 'Use bash to run: echo SAFE_CMD_456'. Wait for result and include the output.",
      "expect_contains": "SAFE_CMD_456"
    }
  ]
}
```

```json
{
  "name": "plan_with_spawn",
  "description": "Create an action plan where one action spawns a child agent",
  "turns": [
    {
      "message": "Create a plan with 2 actions: a1='Spawn a child to compute 2+2' (no deps), a2='Report the result' (depends on a1). Execute the plan and report the final result.",
      "expect_contains": "4"
    }
  ]
}
```

## Non-Goals (Deferred)

- Process groups (`setpgid`) and bulk group signals
- Session detachment (`setsid`) for background agents
- Named pipes (FIFOs) accessible by arbitrary sessions
- Network-transparent IPC across harness instances
- Resource limits (CPU, memory) per child session
