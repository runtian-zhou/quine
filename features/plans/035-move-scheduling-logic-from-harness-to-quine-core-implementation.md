# 035 Move Scheduling Logic from Harness to quine-core — Implementation Plan

Short summary: Move delayed and recurring scheduling orchestration out of `quine-harness` and into `quine-core`, so the core owns schedule semantics and execution ordering while the harness becomes a thin host and IPC adapter.

## Open Questions

- None. This plan resolves the migration to preserve current externally visible behavior in the first iteration: move only the currently implemented harness scheduling and IPC mailbox behavior into `quine-core`, keep `recv_ipc_message(..., non_blocking)` best-effort and non-blocking as it behaves today, preserve launch-time-anchored recurring cadence, preserve equal-deadline FIFO ordering, and preserve current shutdown semantics where pending future scheduled work is dropped after shutdown is dispatched.

## Agreement Status

agreed — QA now matches the latest implementation revision on preserved first-iteration behavior, coverage expectations, and no remaining open questions.

## Proposed Design

- Add a crate-private scheduling runtime in `quine-core` that owns all behavior currently implemented in `LocalHarness::scheduler_loop` and `LocalHarness::schedule_agent`, including:
  - ordered execution by `Instant`
  - FIFO tie-breaking for equal deadlines via a sequence counter
  - recurring re-enqueue for scheduled child-session spawns
  - in-memory IPC mailbox storage and dequeue behavior
  - shutdown handling for pending scheduled work
- Introduce core-owned input variants or an equivalent crate-private command layer so the core can serialize both immediate and delayed work through one runtime boundary. The migrated surface needs to cover the current harness-owned operations:
  - session creation
  - user message delivery
  - tool-result submission
  - interaction responses
  - cancellation and shutdown
  - child-session spawning
  - signals
  - IPC send/receive
  - scheduled delayed message delivery
  - scheduled future or recurring child-session spawns
- Keep the migration scoped to existing observable behavior for the first iteration:
  - preserve the current harness RPC surface where possible
  - preserve immediate-before-delayed ordering
  - preserve equal-deadline FIFO ordering
  - preserve launch-time-based recurring cadence
  - preserve current best-effort IPC receive semantics with `non_blocking` still effectively ignored
  - preserve current shutdown semantics where pending future scheduled work is abandoned once shutdown is dispatched
- Do not conflate this harness IPC mailbox path with the existing core `send_message` tool path. In the current codebase, `quine-core` already defines `CoreInput::SendMessage` and a `SendMessageTool`, but `run_core_loop` treats `CoreInput::SendMessage` as a no-op and `RecvMessageTool` is still a placeholder. This feature should move the harness-owned mailbox implementation into core without accidentally widening scope into full agent mailbox redesign.
- Keep `quine-harness` responsible for:
  - constructing the core runtime
  - forwarding public trait calls to the core input channel
  - broadcasting `CoreOutput` events
  - mapping core errors to harness errors
- Refactor `crates/quine-harness/src/local.rs` so the harness no longer owns `BinaryHeap`, `QueuedCommand`, `ScheduledCommand`, `ScheduledAction`, the spawned recurring-scheduler task logic, or the IPC mailbox `HashMap<String, Vec<String>>`.
- Preserve existing crate boundary rules from `CLAUDE.md` by exposing only the minimal new `quine-core` boundary needed for delegation while keeping scheduler internals crate-private.

## File-by-File Changes

- `crates/quine-core/src/lib.rs`
  - Re-export only any new core-facing construction helpers or public boundary types required by `quine-harness`.
  - Avoid re-exporting concrete scheduler internals.
- `crates/quine-core/src/channel.rs`
  - Add or refactor core input variants so the current harness-owned operations can be represented inside the core runtime with existing `oneshot` reply semantics.
  - Distinguish migrated harness IPC operations from the pre-existing `CoreInput::SendMessage` child-session messaging path so tests can assert the intended behavior.
- `crates/quine-core/src/engine.rs`
  - Integrate a scheduler host into the main event loop, or delegate to a crate-private helper that still feeds one serialized core execution path.
  - Route scheduled delayed user messages through the same code path as immediate `CoreInput::UserMessage` handling.
  - Route scheduled child-session launches through the same child-session spawn path as immediate `CoreInput::SpawnSession` handling.
  - Move IPC mailbox ownership here or into a sibling runtime helper used by the engine.
  - Make the preserved shutdown-drop behavior explicit and testable.
- `crates/quine-core/src/scheduler.rs` or `crates/quine-core/src/runtime/scheduler.rs`
  - New crate-private module for ordered commands, sequence tie-breaking, recurrence metadata, mailbox state, and wakeup logic.
  - Include focused unit tests for ordering, delay handling, recurring enqueue semantics, mailbox dequeue behavior, and shutdown edges.
- `crates/quine-core/src/tool/recv_message.rs`
  - Update only if the migration also connects the existing placeholder receive tool to the moved mailbox implementation.
  - Otherwise leave it unchanged and keep this feature scoped to the harness-owned IPC APIs.
- `crates/quine-core/src/tool/send_message.rs`
  - Update only if necessary to avoid semantic overlap or naming confusion once mailbox ownership moves into core.
  - Do not broaden this refactor into a full redesign of session-to-session tool messaging unless separately agreed.
- `crates/quine-harness/src/local.rs`
  - Remove the harness-owned scheduler loop, queue structs, recurring scheduling task, and mailbox storage.
  - Replace them with thin forwarding into the new core scheduling entry points.
  - Keep event fanout unchanged except for construction changes required by the refactor.
- `crates/quine-harness/src/service.rs`
  - Update documentation comments to reflect that scheduling and mailbox orchestration now live in `quine-core`.
  - Keep method signatures stable unless the core boundary makes a narrow delegation change unavoidable.
- `crates/quine-harness/src/server.rs`
  - Only update if request handling must call renamed harness methods or propagate adjusted error text.
- `crates/quine-harness/src/protocol.rs`
  - Prefer no RPC payload changes; update only if the refactor forces a protocol rename or new error mapping.
- `crates/quine-cli/src/client.rs`
  - Only update if the harness protocol surface changes.
- `crates/quine-cli/src/agent_ctl.rs`
  - Only update if there are user-visible schedule command or error-message changes.
- `CLAUDE.md`
  - Update crate responsibility text only if needed to clarify that scheduling orchestration belongs in `quine-core` rather than `quine-harness`.

## Validation Plan

- Run targeted unit tests for the new `quine-core` scheduling module.
- Add or preserve targeted regression tests covering the current harness-observable behaviors already present in `crates/quine-harness/src/local.rs` tests:
  - delayed message delivery after exact paused-time advancement
  - immediate-before-delayed ordering
  - one-shot scheduled child-session launch behavior
  - IPC send/receive dequeue behavior
- If recurring scheduling remains in scope, add a deterministic recurring test that advances paused time through multiple cadence intervals and verifies repeated child-session launch attempts without long wall-clock sleeps.
- Add a regression assertion that the moved implementation preserves current equal-deadline FIFO ordering, since `LocalHarness` currently enforces that with a sequence counter in `QueuedCommand`.
- Run at least the affected-crate test suites:
  - `cargo test -p quine-core`
  - `cargo test -p quine-harness`
- Run workspace quality gates before handoff if the implementation touches shared APIs:
  - `cargo build`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
- Confirm by repository inspection that `crates/quine-harness/src/local.rs` no longer directly owns scheduling queues, recurring schedule loops, or mailbox orchestration state.

## QA Feedback

- QA should require one daemon-backed smoke test that proves the refactor still works through the real local daemon rather than only direct Rust tests.
- QA should verify not just successful delayed execution but ordering semantics between immediate and delayed work, equal-deadline FIFO ordering, and preservation of launch-time cadence for recurring schedules.
- QA should include at least one scenario that exercises recurring scheduling long enough to observe multiple child-session launches without relying on fragile long sleeps.
- QA should capture evidence that `quine-harness` is only delegating scheduling work after the refactor, for example by validating behavior through public APIs while the relevant queue implementation resides exclusively under `quine-core`.
- QA should include an IPC scenario because the current harness scheduler loop also owns mailbox state; the migration is incomplete if IPC behavior regresses.
- QA evidence should use the daemon’s existing JSON-RPC scheduling and IPC methods for end-to-end coverage. Agent chat that exercises the current `send_message` or `recv_message` tools is not reliable evidence for this feature unless the implementation also wires those still-separate core tool paths into the migrated mailbox runtime.
- QA should treat shutdown behavior as preserved-drop semantics for this feature unless implementation notes explicitly document a separately approved behavior change.
