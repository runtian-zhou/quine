# 045 Tool Permission Integration — QA Plan

Short summary: Verify Quine Feature 3 tool integration with the shared permission engine, including per-tool request construction, conservative classification, and representative allow/deny/ask flows for current built-in tools.

## Open Questions

- None. This plan stays scoped to Feature 3 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest implementation revision and confirmed this QA plan stays aligned with the concrete tool-request, allow/deny/ask, and daemon-backed validation paths.

## Test Strategy

- Test tool-local request construction in colocated unit tests next to each affected tool module so request scope, target extraction, and metadata stay coupled to the concrete parser for that tool.
- Add `quine-core` integration tests that exercise evaluator propagation for representative categories the implementation plan names:
  - read-only filesystem (`read`, `find`)
  - mutating filesystem (`write` via the `apply_patch` tool name)
  - shell execution (`bash`)
  - agent/process control (`spawn`, `subagent`, `signal`)
- Verify runtime parity between static tool availability and runtime enforcement:
  - tools hidden in plan mode remain unavailable even before evaluator decisions
  - tools still exposed in normal sessions surface allow / deny / ask outcomes through normal tool execution results
- Include at least one multi-round daemon-backed chat scenario because this feature changes `quine-core` runtime dispatch, not just pure request construction.
- Prefer `quine test <scenario.toml>` for daemon/chat coverage because `crates/quine-cli/src/test_runner.rs` already supports exact `run`, `respond`, `spawn`, `signal`, `assert`, and output matching flows.

## Scenarios

- **Unit — `read` Request Construction**
  - **Test entry point**: `cargo test -p quine-core tool::read::tests -- --nocapture`
  - **Fixture/input**: execute the `read_file` tool with `{ "file_path": "Cargo.toml" }` in a session rooted at the workspace.
  - **Expected result**:
    - the built permission request resolves `Cargo.toml` against `ExecutionContext.working_directory`
    - the request is classified as a read-scoped filesystem action
    - the tool remains non-mutating in metadata and does not construct a write or execute request

- **Unit — `find` Request Construction**
  - **Test entry point**: `cargo test -p quine-core tool::find::tests -- --nocapture`
  - **Fixture/input**: execute the `find` tool with `{ "path": ".", "pattern": "*.rs", "type": "file" }`.
  - **Expected result**:
    - the built permission request uses the requested root path after session-filesystem resolution
    - the request is classified as read-scoped traversal/search
    - no write, execute, or agent-control classification appears

- **Unit — `write` / `apply_patch` Request Construction**
  - **Test entry point**: `cargo test -p quine-core tool::write::tests -- --nocapture`
  - **Fixture/input**: execute the `apply_patch` tool name with a patch targeting `scratch/permission-check.txt`.
  - **Expected result**:
    - the built permission request identifies the final target path `scratch/permission-check.txt`
    - the request is classified as write-scoped and mutating
    - an allow decision preserves the existing success-shaped tool output, while a deny decision maps to `ToolError::PermissionDenied`

- **Unit — `bash` Request Construction**
  - **Test entry point**: `cargo test -p quine-core tool::bash::tests -- --nocapture`
  - **Fixture/input**: execute `bash` with `{ "command": "printf qa-bash-check" }`.
  - **Expected result**:
    - the built permission request is execute-scoped
    - the raw command text is preserved in request metadata for later policy evaluation
    - the request is not misclassified as filesystem-read or write-only

- **Unit — Agent/Process-Control Request Construction**
  - **Test entry points**:
    - `cargo test -p quine-core tool::spawn::tests -- --nocapture`
    - `cargo test -p quine-core tool::subagent::tests -- --nocapture`
    - `cargo test -p quine-core tool::signal::tests -- --nocapture`
  - **Fixture/input**:
    - `spawn` with a simple task string
    - `subagent` with a simple task string
    - `signal` with a target child session ID and `term`
  - **Expected result**:
    - each tool constructs a process-control or agent-control permission request, not a filesystem or shell request
    - deny/ask decisions stop the underlying control action before it executes

- **Integration — Evaluator Propagation for Allow / Deny / Ask**
  - **Test entry point**: `cargo test -p quine-core --test tool_permission_integration -- --nocapture`
  - **Fixture/setup**: add one integration test file that installs a deterministic permission policy stub with three cases:
    - **allow case**: `read_file` on `Cargo.toml`
    - **deny case**: `apply_patch` editing `scratch/deny.txt`
    - **ask case**: `spawn` creating a child session
  - **Expected result**:
    - allow case returns normal tool success output and no permission error
    - deny case returns `ToolError::PermissionDenied` and leaves the target file unchanged
    - ask case surfaces the runtime’s interaction/pending-permission path rather than silently executing the tool

- **Daemon Multi-Round — `bash` Ask Then Approve**
  - **How to start the daemon**:
    - terminal 1: `cargo run --bin quine -- daemon start --socket /tmp/quine-045.sock`
  - **How to run the scenario**:
    - terminal 2: `cargo run --bin quine -- run --json --socket /tmp/quine-045.sock "Use the bash tool to run exactly: printf qa-bash-approval"`
    - capture the `session_id` from the JSON output
    - terminal 2: `cargo run --bin quine -- respond --json --socket /tmp/quine-045.sock --session <session_id> "approve"`
  - **Round-by-round messages**:
    - **Round 1 user message**: `Use the bash tool to run exactly: printf qa-bash-approval`
    - **Expected Round 1 result**:
      - stdout JSON or stderr text indicates an interaction is needed before completion
      - tool activity shows a `bash` request was attempted
      - the output includes an interaction prompt whose meaning is equivalent to approving or denying the shell command
      - no final assistant text claims the command already executed
    - **Round 2 user message**: `approve`
    - **Expected Round 2 result**:
      - the session completes without another permission prompt for the same command
      - tool activity still shows the `bash` call for this turn
      - final assistant text contains `qa-bash-approval` or explicitly reports that the `printf qa-bash-approval` command succeeded

- **Daemon Multi-Round — `apply_patch` Deny**
  - **How to start the daemon**:
    - terminal 1: `cargo run --bin quine -- daemon start --socket /tmp/quine-045.sock`
  - **How to run the scenario**:
    - terminal 2: `cargo run --bin quine -- run --json --socket /tmp/quine-045.sock "Use apply_patch to create scratch/qa-denied.txt containing DENIED_WRITE_TEST"`
    - capture the `session_id` from the JSON output
    - terminal 2: `cargo run --bin quine -- respond --json --socket /tmp/quine-045.sock --session <session_id> "deny"`
  - **Round-by-round messages**:
    - **Round 1 user message**: `Use apply_patch to create scratch/qa-denied.txt containing DENIED_WRITE_TEST`
    - **Expected Round 1 result**:
      - stdout JSON or stderr text indicates an interaction is needed before completion
      - tool activity shows an `apply_patch` request was attempted
      - the output includes a permission/approval prompt for a write action
    - **Round 2 user message**: `deny`
    - **Expected Round 2 result**:
      - final assistant text states that the write was denied, cancelled, or could not proceed because permission was not granted
      - `scratch/qa-denied.txt` does not exist after the turn
      - no success text claims the patch was applied

- **Daemon One-Shot — Read Allowed Without Prompt**
  - **How to start the daemon**:
    - terminal 1: `cargo run --bin quine -- daemon start --socket /tmp/quine-045.sock`
  - **Command**:
    - terminal 2: `cargo run --bin quine -- run --json --socket /tmp/quine-045.sock "Use read_file on Cargo.toml and tell me the package name from the workspace manifest."`
  - **Expected result**:
    - the command completes in one turn without `interaction_needed`
    - tool activity includes `read_file`
    - final assistant text references content from `Cargo.toml` rather than a permission denial

- **Daemon One-Shot — Plan Mode Availability Parity**
  - **How to start the daemon and connect**:
    - terminal 1: `cargo run --bin quine -- daemon start --socket /tmp/quine-045.sock`
    - terminal 2: `cargo run --bin quine -- chat --plan --socket /tmp/quine-045.sock`
  - **Exact chat message**:
    - `Use apply_patch to create scratch/plan-mode-should-not-run.txt with the text PLAN_MODE_BLOCKED.`
  - **Expected result**:
    - the assistant does not execute `apply_patch`
    - the response explains that plan mode cannot perform the requested mutating action or offers a non-executing plan instead
    - this confirms plan-mode tool availability still blocks the tool before runtime permission enforcement can drift from the static tool list

## Required Evidence

- Passing colocated unit-test output or test names for:
  - `tool::read::tests`
  - `tool::find::tests`
  - `tool::write::tests`
  - `tool::bash::tests`
  - `tool::spawn::tests`, `tool::subagent::tests`, and `tool::signal::tests`
- Passing integration-test output for the representative allow / deny / ask file described above, including evidence that a deny case returns `ToolError::PermissionDenied` and leaves side effects unapplied.
- Daemon-backed evidence for all three runtime behaviors:
  - `bash` ask-then-approve multi-round transcript or `quine test` scenario result
  - `apply_patch` ask-then-deny multi-round transcript or `quine test` scenario result
  - one-turn `read_file` allow transcript showing no interaction prompt
- Plan-mode parity evidence showing a mutating tool remains unavailable in `quine chat --plan`.
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Reviewed the paired implementation plan’s latest revision. Its scope, file targeting, and validation categories align with this QA plan.
- This QA doc now incorporates the concrete revisions requested in the implementation doc’s `## QA Feedback` section:
  - exact unit-test entry points by tool category
  - exact daemon start/connect commands
  - explicit multi-round chat scenarios for `bash` and `apply_patch`
  - explicit allow / deny / ask coverage with observable outcomes
- No additional implementation-side changes are required from QA at this time.
- Next coordination step: the implementation plan should re-review this updated QA revision and, if it agrees these scenarios are sufficiently concrete, update its own `## Agreement Status` to `agreed` so both docs can converge.
