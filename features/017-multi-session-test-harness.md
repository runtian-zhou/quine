---
status: in-progress
---

# Multi-Session Test Harness for Interactive & Multi-Agent Tools

## Overview

The current QA workflow (`/qa` skill) can only send one-off messages and check output. This makes it impossible to test interactive tools (`ask_user`) and multi-agent tools (`spawn`/`wait_child`/`signal`, `send_message`/`recv_message`) which require orchestrating multiple sessions or responding to interaction prompts mid-turn.

This feature adds a `quine test` CLI command that runs scripted multi-session test scenarios, enabling full coverage of all implemented tools.

## Requirements

### 1. New CLI Command: `quine test`

Add a `Test` variant to `Commands` in `crates/quine-cli/src/main.rs`:

```rust
/// Run a scripted test scenario against the daemon.
Test {
    /// Path to a test scenario TOML file, or a directory of scenarios.
    scenario: String,
    /// Socket path to connect to the harness daemon.
    #[arg(long)]
    socket: Option<String>,
    /// Output results as JSON.
    #[arg(long)]
    json: bool,
}
```

### 2. Test Scenario File Format

Each scenario is a TOML file describing a sequence of **actions** executed against the daemon. Actions run sequentially by default. The format:

```toml
[meta]
name = "ask_user_interaction"
description = "Verify ask_user pauses and resumes on interaction response"
timeout = 120  # seconds, for the entire scenario

[[action]]
type = "run"
message = "Use the ask_user tool to ask: 'What is your name?'"
expect_interaction = true       # expect interaction_needed, not turn_complete
save_session = "sid"            # save session_id to variable "sid"

[[action]]
type = "respond"
session = "$sid"                # reference saved variable
response = "Alice"
expect_contains = "Alice"       # expect the agent to use the response

[[action]]
type = "run"
message = "Use the spawn tool to create a child agent with the task: 'Use the bash tool to run echo CHILD_DONE and return the output.'"
flags = ["--json"]
save_session = "parent_sid"
extract_json = { field = "response", regex = "SessionId\\(([^)]+)\\)", save = "child_id" }

[[action]]
type = "ps"
expect_contains = "$child_id"   # child should appear in process list

[[action]]
type = "run"
session = "$parent_sid"
message = "Use wait_child to collect the result of the child session you just spawned."
expect_contains = "CHILD_DONE"
```

**Action types:**

| Type | Description | Key Fields |
|------|-------------|------------|
| `run` | Send a one-off message (`quine run`) | `message`, `session`, `flags`, `expect_contains`, `expect_interaction`, `save_session`, `extract_json` |
| `respond` | Respond to interaction (`quine respond`) | `session`, `response`, `expect_contains` |
| `spawn` | Spawn child session (`quine spawn`) | `task`, `parent`, `system_prompt`, `save_session` |
| `signal` | Send signal to session (`quine signal`) | `session`, `signal` (term/kill/stop/continue) |
| `send` | Send IPC message (`quine send`) | `target`, `message` |
| `recv` | Receive IPC message (`quine recv`) | `source`, `non_blocking`, `expect_contains` |
| `ps` | List sessions (`quine ps`) | `expect_contains`, `flags` |
| `sleep` | Wait for a duration | `seconds` |
| `assert` | Assert a saved variable matches | `var`, `contains` or `equals` |

**Variable interpolation:** Any field value containing `$varname` is replaced with the saved variable's value.

### 3. Implementation: `crates/quine-cli/src/test_runner.rs`

New module implementing the test runner:

```rust
pub struct TestRunner {
    client_factory: Box<dyn Fn() -> IpcClient>,  // or socket_path
    variables: HashMap<String, String>,
}

pub struct ScenarioResult {
    pub name: String,
    pub passed: bool,
    pub actions: Vec<ActionResult>,
    pub duration_ms: u64,
}

pub struct ActionResult {
    pub action_index: usize,
    pub action_type: String,
    pub passed: bool,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}

impl TestRunner {
    pub async fn run_scenario(&mut self, scenario: &Scenario) -> ScenarioResult;
}
```

Each action type maps to an existing CLI function:
- `run` → calls `run_oneshot()` (or shells out to `quine run`)
- `respond` → calls `run_respond()` (or shells out to `quine respond`)
- `spawn` → calls `handle_spawn()` via IPC
- `signal` → calls `handle_signal()` via IPC
- `send` → calls `handle_send()` via IPC
- `recv` → calls `handle_recv()` via IPC
- `ps` → calls `handle_ps()` via IPC

The simplest implementation shells out to `cargo run --bin quine -- <subcommand>` and captures stdout/stderr, reusing all existing CLI plumbing without duplication.

### 4. Test Scenario Files

Create `qa/scenarios/` directory with these scenarios:

#### `qa/scenarios/ask_user.toml`
Tests `ask_user` tool interaction flow:
1. Send message that triggers `ask_user` → expect `interaction_needed`
2. Respond with an answer → expect agent uses the answer

#### `qa/scenarios/spawn_wait.toml`
Tests `spawn` + `wait_child`:
1. Send message that spawns a child via `spawn` tool
2. Send follow-up to `wait_child` for the spawned child
3. Verify child result is collected

#### `qa/scenarios/spawn_signal.toml`
Tests `spawn` + `signal`:
1. Spawn a long-running child (e.g., `sleep 60`)
2. Signal it with `term`
3. `wait_child` should show terminated status

#### `qa/scenarios/ipc_messaging.toml`
Tests `send_message` + `recv_message`:
1. Spawn two child sessions
2. Have session A send a message to session B
3. Have session B receive the message
4. Verify message content matches

#### `qa/scenarios/session_tree.toml`
Tests `spawn` + `ps --tree`:
1. Spawn a parent with a child
2. Run `ps --tree`
3. Verify tree shows parent-child hierarchy

### 5. Integration with `/qa` Skill

Update `.claude/commands/qa.md` to optionally run scenario files:
- `/qa` (no args) runs the existing one-off test suite from `.claude/qa-tests.md`
- `/qa scenarios` runs all TOML scenario files from `qa/scenarios/`
- `/qa scenario:ask_user` runs a specific scenario

Update `.claude/qa-tests.md` header to document this.

## Acceptance Criteria

- `cargo build` / `cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --all -- --check` must pass
- `quine test qa/scenarios/ask_user.toml --socket /tmp/quine-qa.sock` executes the scenario and reports pass/fail
- `quine test qa/scenarios/ --socket /tmp/quine-qa.sock` runs all scenarios in the directory
- `--json` flag outputs structured results
- Unit tests:
  - TOML scenario parsing and variable interpolation
  - Action result evaluation (expect_contains, expect_interaction)
- Existing tests must continue to pass

## QA Test Cases

After implementation, add these to `.claude/qa-tests.md`:

```
## ask_user_interaction
**Description**: Verify ask_user pauses execution and resumes with user response.
- **Scenario**: `qa/scenarios/ask_user.toml`
- **Expect**: Scenario passes (agent uses the provided response)

## spawn_and_wait_child
**Description**: Verify spawn creates a child and wait_child collects the result.
- **Scenario**: `qa/scenarios/spawn_wait.toml`
- **Expect**: Scenario passes (child result is collected by parent)

## signal_child
**Description**: Verify signal terminates a running child session.
- **Scenario**: `qa/scenarios/spawn_signal.toml`
- **Expect**: Scenario passes (child is terminated)

## ipc_send_recv
**Description**: Verify send_message and recv_message between sessions.
- **Scenario**: `qa/scenarios/ipc_messaging.toml`
- **Expect**: Scenario passes (message content matches)
```

## Non-Goals (Deferred)

- Parallel action execution within a scenario (sequential is sufficient for now)
- Scenario composition / imports between files
- Performance benchmarking or load testing
- GUI test runner
- Fixing `recv_message` placeholder implementation (separate feature — this feature assumes it will be implemented)
