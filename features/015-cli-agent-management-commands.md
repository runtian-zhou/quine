---
status: done
---

# CLI Agent Management Commands

## Overview

Expose the agent process control syscalls defined in `features/013-agent-process-control-and-ipc.md` as top-level CLI commands, analogous to how Linux exposes syscalls via commands like `kill` and `ps`. This gives users (and scripts) direct control over agent sessions from the shell without entering an interactive chat session.

Additionally, add a `ps` command that lists all current agent sessions with their status, parent-child relationships, and uptime — similar to `ps aux` on Linux.

## Requirements

### 1. New CLI Subcommands (`crates/quine-cli/src/main.rs`)

Add the following variants to the `Commands` enum (clap derive):

```rust
/// List agent sessions (like `ps`)
Ps {
    /// Show all sessions including destroyed (like `ps -a`)
    #[arg(short, long)]
    all: bool,
    /// Show tree view with parent-child relationships (like `pstree`)
    #[arg(short, long)]
    tree: bool,
    /// Output as JSON
    #[arg(long)]
    json: bool,
},

/// Spawn a new child agent session (like `fork+exec`)
Spawn {
    /// Task description for the child agent
    task: String,
    /// Parent session ID (if omitted, creates a root session)
    #[arg(short, long)]
    parent: Option<String>,
    /// Custom system prompt for the child
    #[arg(long)]
    system_prompt: Option<String>,
    /// Inherit conversation history from parent
    #[arg(long, default_value_t = false)]
    inherit_history: bool,
    /// Output as JSON
    #[arg(long)]
    json: bool,
},

/// Send a signal to an agent session (like `kill`)
Signal {
    /// Session ID to signal
    session_id: String,
    /// Signal to send: term, kill, stop, continue
    #[arg(short, long, default_value = "term")]
    signal: String,
},

/// Send a message to an agent session (like `write`)
Send {
    /// Target session ID or pipe ID
    target: String,
    /// Message content (reads from stdin if omitted)
    #[arg(short, long)]
    message: Option<String>,
    /// Shared memory key (if targeting shared memory)
    #[arg(short, long)]
    key: Option<String>,
},

/// Receive a message from an agent session (like `read`)
Recv {
    /// Source: session ID, pipe ID, or "any"
    source: String,
    /// Non-blocking read
    #[arg(short, long)]
    non_blocking: bool,
    /// Shared memory key (if reading from shared memory)
    #[arg(short, long)]
    key: Option<String>,
    /// Output as JSON
    #[arg(long)]
    json: bool,
},
```

### 2. Harness Protocol Extensions (`crates/quine-harness/src/protocol.rs`)

Add new RPC method constants to the `methods` module:

```rust
pub const LIST_SESSIONS: &str = "list_sessions";   // already exists
pub const SPAWN_SESSION: &str = "spawn_session";
pub const SIGNAL_SESSION: &str = "signal_session";
pub const SEND_IPC_MESSAGE: &str = "send_ipc_message";
pub const RECV_IPC_MESSAGE: &str = "recv_ipc_message";
```

### 3. Harness Service Extensions (`crates/quine-harness/src/service.rs`)

Extend the `HarnessService` trait with new methods:

```rust
async fn spawn_session(
    &self,
    parent_id: Option<SessionId>,
    task: String,
    system_prompt: Option<String>,
    inheritance: InheritanceFlags,
) -> Result<SessionId>;

async fn signal_session(
    &self,
    session_id: SessionId,
    signal: SessionSignal,
) -> Result<()>;

async fn send_ipc_message(
    &self,
    target: String,
    content: String,
    key: Option<String>,
) -> Result<()>;

async fn recv_ipc_message(
    &self,
    source: String,
    non_blocking: bool,
    key: Option<String>,
) -> Result<Option<String>>;
```

### 4. Server Request Routing (`crates/quine-harness/src/server.rs`)

Add match arms in `handle_request` for the new methods, deserializing JSON params and forwarding to the corresponding `HarnessService` methods. Follow the existing pattern used for `create_session`, `send_message`, etc.

### 5. `list_sessions` Response Enhancement (`crates/quine-harness/src/service.rs`)

The existing `list_sessions` method needs to return richer data for the `ps` command:

```rust
#[derive(Serialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub state: SessionState,
    pub parent_id: Option<SessionId>,
    pub children: Vec<SessionId>,
    pub created_at: DateTime<Utc>,
    pub task_summary: Option<String>,  // first ~80 chars of the initial task
}
```

### 6. CLI Handler Module (`crates/quine-cli/src/agent_ctl.rs`)

New module containing handler functions for each command. Each handler:
1. Connects to the daemon via `IpcClient::connect_or_launch()`
2. Sends the appropriate JSON-RPC request
3. Formats and prints the response

**`handle_ps`**: Calls `list_sessions`, formats as a table:
```
SESSION ID                            STATE      PARENT    TASK
a1b2c3d4-...                          Streaming  -         Implement feature X...
e5f6a7b8-...                          Idle       a1b2c3d4  Review code changes...
```

With `--tree` flag, render indented tree:
```
a1b2c3d4  Streaming  Implement feature X...
├─ e5f6a7b8  Idle       Review code changes...
└─ c9d0e1f2  Idle       Run tests...
```

With `--json` flag, output raw JSON array.

With `--all` flag, include sessions in `Destroyed` state.

**`handle_spawn`**: Calls `spawn_session`, prints the new session ID. With `--json`, outputs `{"session_id": "..."}`.

**`handle_signal`**: Calls `signal_session`. Parses signal string to `SessionSignal` enum (term/kill/stop/continue). Prints confirmation.

**`handle_send`**: Calls `send_ipc_message`. If `--message` is omitted, reads content from stdin (enables piping: `echo "do X" | quine send <id>`).

**`handle_recv`**: Calls `recv_ipc_message`. Prints received message content to stdout (enables piping: `quine recv <id> | jq .`).

### 7. Module Registration (`crates/quine-cli/src/main.rs`)

Add `mod agent_ctl;` and route the new command variants to handler functions in `agent_ctl.rs`. Follow the existing pattern where `Commands::Chat` dispatches to `chat::run_chat()`, etc.

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass
- Existing tests continue to pass
- New unit tests required:
  - `parse_signal_string` — validate "term", "kill", "stop", "continue" parsing and invalid input error
  - `format_ps_table` — verify table formatting with sample `SessionInfo` data
  - `format_ps_tree` — verify tree rendering with nested parent-child relationships
  - `spawn_json_output` — verify JSON output format for spawn command
  - `send_reads_stdin` — verify message is read from stdin when `--message` is omitted
- Integration tests:
  - `cli_ps_lists_sessions` — start daemon, create sessions, run `quine ps`, verify output contains session IDs
  - `cli_signal_term` — spawn a session, send `quine signal <id> -s term`, verify with `quine ps` that it stopped
  - `cli_send_recv_roundtrip` — spawn session, send message via `quine send`, receive via `quine recv`

## QA Test Cases (add to `qa/test_cases.json`)

```json
[
  {
    "name": "cli_ps_command",
    "description": "quine ps lists active agent sessions with status",
    "steps": [
      "Start a chat session in background: `quine run 'think for a while' &`",
      "Run `quine ps`",
      "Verify output shows the session ID and its state (Streaming or Idle)",
      "Run `quine ps --json`",
      "Verify output is valid JSON array with session objects"
    ]
  },
  {
    "name": "cli_ps_tree_view",
    "description": "quine ps --tree shows parent-child hierarchy",
    "steps": [
      "Spawn a parent session: `quine spawn 'spawn a child to say hello'`",
      "Run `quine ps --tree`",
      "Verify indented tree output shows parent-child relationship"
    ]
  },
  {
    "name": "cli_spawn_and_check",
    "description": "Spawn a child agent from CLI and check its status",
    "steps": [
      "Run `quine spawn 'Say exactly: CLI_SPAWN_TEST_123' --json`",
      "Capture session_id from JSON output",
      "Run `quine ps --json`",
      "Verify the session appears in the output with its state"
    ]
  },
  {
    "name": "cli_signal_term",
    "description": "Send SIGTERM to a running agent via CLI",
    "steps": [
      "Run `quine spawn 'Count from 1 to 1000000 slowly'`",
      "Capture the session ID",
      "Run `quine signal <session_id> -s term`",
      "Run `quine ps` and verify session state reflects termination"
    ]
  },
  {
    "name": "cli_send_recv_pipe",
    "description": "Send and receive messages between CLI and agent using shell pipes",
    "steps": [
      "Spawn a session: `quine spawn 'Use recv_message to read from any source, then repeat the message'`",
      "Run `echo 'PIPE_MSG_456' | quine send <session_id>`",
      "Run `quine recv <session_id>` to read the response"
    ]
  }
]
```

## Non-Goals (Deferred)

- Shell completion scripts for session IDs (e.g., tab-complete session IDs in `quine signal <TAB>`)
- `quine top` — real-time dashboard of agent activity (like `htop`)
- `quine attach` — attach to a running session's streaming output interactively
- `quine pipe` — create named pipes between sessions from CLI
- Shared memory CLI commands (`quine shm-read`, `quine shm-write`) — can be added later if needed
