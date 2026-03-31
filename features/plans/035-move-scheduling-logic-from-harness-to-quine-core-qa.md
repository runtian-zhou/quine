# 035 Move Scheduling Logic from Harness to quine-core — QA Plan

Short summary: Verify that moving scheduling orchestration from `quine-harness` into `quine-core` preserves observable behavior for delayed messages, scheduled child-session launches, IPC mailbox operations, and daemon-backed chat flows while making the harness a thin adapter.

## Open Questions

- None. QA now aligns on preserving current behavior for the first iteration: keep the existing external scheduling API stable where possible, preserve launch-time-anchored recurring cadence, preserve equal-deadline FIFO ordering, keep `recv_ipc_message(..., non_blocking)` behaviorally unchanged as an immediate best-effort dequeue, and preserve the current shutdown behavior where pending future scheduled work is dropped after shutdown is dispatched.

## Agreement Status

agreed — re-review confirms the implementation plan matches the latest QA expectations on preserved first-iteration behavior, coverage, and no remaining open questions.

## Test Strategy

- Prefer deterministic unit and integration tests with paused Tokio time for queue ordering, delayed execution, recurrence, and shutdown edges.
- Validate behavior at three levels:
  - focused `quine-core` unit tests for the new scheduler/mailbox runtime
  - `quine-harness` regression tests proving the public harness surface still behaves the same
  - one daemon-backed smoke path through the real JSON-RPC service for externally visible scheduling behavior
- Focus on regression coverage for the behavior currently implemented in `crates/quine-harness/src/local.rs`:
  - delayed message scheduling through the same serialized path as immediate messages
  - immediate-before-delayed ordering
  - equal-deadline FIFO ordering
  - one-shot and recurring scheduled child-session launching
  - IPC mailbox enqueue/dequeue behavior
  - shutdown behavior for pending scheduled work
- Keep mailbox QA scoped to the harness-owned IPC APIs and migration target, not the existing in-core agent tool path. Today `quine-core` still treats `CoreInput::SendMessage` as a no-op and `RecvMessageTool` returns a placeholder `null`, so tool-driven `send_message`/`recv_message` chat scenarios are not valid evidence for this feature unless the implementation explicitly wires them into the moved mailbox runtime.
- Confirm the refactor does not introduce crate-boundary drift by checking that scheduling semantics are tested from `quine-core` and only delegated from `quine-harness`.

## Scenarios

- **Scenario 1: Core queue ordering unit tests**
  - Start/use: run crate-local tests only; no daemon required.
  - Command: `cargo test -p quine-core scheduler`
  - Exact validation: verify a command scheduled for `t+30s` does not run before a command at `t+0s`, and verify equal-deadline commands preserve insertion order via the migrated equivalent of the current harness `QueuedCommand.sequence` contract.
  - Expected result: all scheduler ordering tests pass with no wall-clock sleeps.

- **Scenario 2: Delayed message integration via harness tests**
  - Start/use: run harness tests with paused Tokio time.
  - Command: `cargo test -p quine-harness local_harness_scheduled_message_runs_after_delay`
  - Exact validation: before advancing time, no `TextComplete` event is emitted for the delayed message; after advancing exactly the configured delay, one `TextComplete` event appears with `full_text` equal to `scheduled`.
  - Expected result: the message fires only after the delay and produces the same text as before the refactor.

- **Scenario 3: Immediate-before-delayed ordering integration**
  - Start/use: run harness tests with paused Tokio time.
  - Command: `cargo test -p quine-harness local_harness_orders_immediate_before_delayed_message`
  - Exact validation: an immediate `send_message("now")` completes before a delayed scheduled message `later`, and the observed completion order is exactly `["now", "later"]`.
  - Expected result: test passes and proves the new core scheduler preserves ordering.

- **Scenario 4: Scheduled child-session one-shot regression**
  - Start/use: run targeted harness or core-backed integration test for one-shot scheduling.
  - Command: `cargo test -p quine-harness local_harness_schedule_agent_one_shot`
  - Exact validation: scheduling a one-shot agent returns success and shutdown remains clean; the preferred strengthened assertion is that a `ChildSpawned` event is observed after advancing paused time rather than merely asserting no panic.
  - Expected result: no regression in public scheduling API and no panics during shutdown.

- **Scenario 5: IPC mailbox regression**
  - Start/use: run targeted harness tests.
  - Command: `cargo test -p quine-harness local_harness_recvs_ipc_message`
  - Exact validation: after `send_ipc_message("worker", "payload")`, a matching `recv_ipc_message("worker", false)` returns `Some("payload")`.
  - Expected result: mailbox behavior remains unchanged after ownership moves to `quine-core`.

- **Scenario 6: Local daemon JSON-RPC schedule-agent smoke test**
  - Start/use: in one terminal, start the local daemon. Command: `cargo run --bin quine-harness -- --socket /tmp/quine-scheduler.sock`
  - Exercise/use: in a second terminal, use the existing daemon surface rather than agent-tool chat to hit the real scheduling API.
  - Exact commands to send:
    - `printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"create_session","params":{}}' | socat - UNIX-CONNECT:/tmp/quine-scheduler.sock`
    - Capture the returned `session_id`.
    - `printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"schedule_agent","params":{"parent_id":"<session_id>","task":"scheduled child task","delay_ms":50}}' | socat - UNIX-CONNECT:/tmp/quine-scheduler.sock`
  - Expected output:
    - `create_session` returns a JSON-RPC success response containing a valid `session_id` string and no error.
    - `schedule_agent` returns a JSON-RPC success response with a success result and no error text.
    - While the daemon remains running, notifications or session-log evidence show the delayed child-session work was accepted and later dispatched without harness crash.
  - Expected result: the daemon-backed public scheduling path still works end-to-end after the move.

- **Scenario 7: Local daemon JSON-RPC IPC smoke test**
  - Start/use: start daemon with `cargo run --bin quine-harness -- --socket /tmp/quine-scheduler.sock`
  - Exercise/use: send JSON-RPC IPC methods against the daemon socket.
  - Exact commands to send:
    - `printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"send_ipc_message","params":{"target":"worker","content":"hello-mailbox"}}' | socat - UNIX-CONNECT:/tmp/quine-scheduler.sock`
    - `printf '%s\n' '{"jsonrpc":"2.0","id":4,"method":"recv_ipc_message","params":{"source":"worker","non_blocking":false}}' | socat - UNIX-CONNECT:/tmp/quine-scheduler.sock`
  - Expected output:
    - `send_ipc_message` returns a JSON-RPC success response with no error text.
    - `recv_ipc_message` returns a JSON-RPC success response whose result is `"hello-mailbox"` or the equivalent wrapped success payload defined by the protocol.
    - A follow-up receive with the same source returns `null` or the equivalent empty-result payload, matching current best-effort dequeue semantics.
  - Expected result: moving mailbox ownership into `quine-core` does not break daemon-exposed IPC behavior.

- **Scenario 8: Recurring schedule cadence regression**
  - Start/use: run a deterministic paused-time test in `quine-core` or `quine-harness`, depending on where the final regression coverage lands.
  - Command: `cargo test -p quine-core recurring` or equivalent targeted test name.
  - Exact validation: advancing time through multiple cadence intervals produces multiple child-session launch attempts, and the observed timing matches the agreed cadence anchor rule instead of silently changing semantics during the migration.
  - Expected result: recurring scheduling semantics are explicit, deterministic, and preserved or intentionally updated.

- **Scenario 9: Shutdown edge regression**
  - Start/use: run a deterministic paused-time test around runtime shutdown with pending scheduled work.
  - Command: `cargo test -p quine-core shutdown` or equivalent targeted test name.
  - Exact validation: after scheduling future work and issuing shutdown, no future pending scheduled work is dispatched after shutdown is forwarded; the runtime exits cleanly and matches the preserved current contract of dropping pending future work.
  - Expected result: shutdown semantics are explicit, testable, and preserved.

## Required Evidence

- Targeted passing tests for moved scheduler behavior in `quine-core`, including ordering, recurrence, mailbox behavior, and shutdown edges.
- Passing regression tests in `quine-harness` for delayed messages, ordering, scheduled child-session behavior, and IPC.
- Evidence from at least one local daemon JSON-RPC smoke path touching the refactored public scheduling surface and one daemon-exposed IPC path.
- Passing workspace checks:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
- Repository inspection evidence that queueing and mailbox orchestration no longer live in `crates/quine-harness/src/local.rs`.

## Implementation Feedback

- The implementation plan should explicitly call out migration of IPC mailbox state alongside delayed scheduling, because both currently live inside the same harness scheduler loop.
- The implementation plan should define the shutdown contract for pending scheduled work so QA can write an exact expectation.
- The implementation plan should prefer targeted preservation of the existing harness API, because that makes regression testing substantially clearer.
- If the implementation adds a new core scheduler module, it should expose deterministic seams for paused-time testing rather than burying all behavior directly in `engine.rs`.
- The implementation plan should document that equal-deadline ordering is currently FIFO via the harness `QueuedCommand` sequence counter, so QA can treat tie-order preservation as an intentional regression check.
- The implementation plan should note that current harness IPC/mailbox behavior is separate from the existing in-core `send_message` tool path, because QA otherwise risks testing the wrong messaging flow.
- Daemon-level QA should be framed around the existing harness JSON-RPC methods such as `schedule_agent`, `send_ipc_message`, and `recv_ipc_message`, not around agent chat prompts that invoke the current `send_message` or `recv_message` tools, because those tool paths are still no-op or placeholder in today’s `quine-core`.
