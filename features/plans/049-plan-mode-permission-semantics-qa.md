# 049 Plan Mode Permission Semantics — QA Plan

Short summary: Verify Quine Feature 7 plan-mode permission semantics, including explicit mode transitions, prior-mode restoration, and differentiated treatment of mutating versus read-only tool requests while in plan mode.

## Open Questions

- None. This plan stays scoped to Feature 7 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest `features/plans/049-plan-mode-permission-semantics-implementation.md` revision and confirmed the paired docs now match on explicit mode transitions, persisted `plan_mode` compatibility, concrete daemon-backed plan/exit behavior, and differentiated read-versus-mutating outcomes. Both docs are aligned and have no unresolved open questions.

## Test Strategy

- Cover the feature at three layers:
  - `quine-core` unit and integration tests for explicit mode transitions and evaluator behavior
  - session/bootstrap tests for `plan_mode: true` initialization plus `EXIT_PLAN_MODE` restoration behavior
  - one real local-daemon multi-round chat scenario through the existing CLI flow because plan mode is user-visible in `quine-cli`
- Treat the implementation plan's contract as the test boundary:
  - entering `Plan` stores the prior mode exactly once
  - leaving `Plan` restores the prior mode deterministically and clears transitional state
  - repeated entry into `Plan` does not overwrite the original prior mode
  - read-only tool requests still execute in plan mode
  - mutating/process/agent-control requests take the implementation's explicit `ask` or `deny` path in plan mode
- Prefer machine-verifiable evidence over subjective transcript review: exact `cargo test` targets, JSON/session-log assertions where available, and deterministic CLI text for the explicit exit flow.

## Scenarios

- **Unit — Enter plan mode stores the prior mode once**
  - Command:
    - `cargo test -p quine-core permission:: -- --nocapture`
  - Required automated test case to add/run:
    - construct a permission context in a non-`Plan` mode such as the default interactive mode used elsewhere in the permission subsystem
    - call the new plan-entry helper once
    - call the same plan-entry helper a second time without leaving `Plan`
  - Expected result from the test:
    - after the first entry, `current_mode == Plan`
    - `pre_plan_mode` equals the original non-`Plan` mode
    - after the second entry, `current_mode` is still `Plan`
    - `pre_plan_mode` is unchanged from the original saved mode rather than being overwritten with `Plan`
    - the test passes without panics and without any secondary transition state being introduced
- **Unit — Exit plan mode restores the saved mode and clears transitional state**
  - Command:
    - `cargo test -p quine-core permission:: -- --nocapture`
  - Required automated test case to add/run:
    - start from a context that has already entered `Plan` from a known original mode
    - call the plan-exit helper exactly once
  - Expected result from the test:
    - `current_mode` equals the original saved mode
    - `pre_plan_mode` is cleared after restoration
    - a second exit attempt is deterministic according to the implementation contract and does not corrupt state
    - the test output shows pass/fail only; no ignored or flaky behavior is acceptable
- **Unit — Session bootstrap maps persisted `plan_mode: true` into permission context**
  - Command:
    - `cargo test -p quine-core plan_mode -- --nocapture`
  - Required automated test case to add/run:
    - create or resume a session through the same bootstrap path used by persisted session config with `plan_mode: true`
  - Expected result from the test:
    - the restored runtime session starts with plan mode enabled in both the compatibility boolean surface and the explicit permission mode state
    - the saved prior mode is initialized consistently with the implementation design rather than left ambiguous or unset incorrectly
    - the test proves the feature works through bootstrap rather than only through direct helper invocation
- **Integration — Read-only tool request succeeds in plan mode**
  - Command:
    - `cargo test -p quine-core plan_mode_read -- --nocapture`
  - Required automated test case to add/run:
    - create a plan-mode session through the normal engine/bootstrap path
    - drive one turn that issues a representative read-only tool request already allowed by plan-mode tool filtering, such as listing files or reading a file
  - Expected result from the test:
    - the read-only tool request is permitted without an extra permission prompt
    - the tool completes successfully
    - the assistant turn completes normally
    - the recorded tool activity shows the read-only tool actually ran rather than being silently filtered out or denied
- **Integration — Mutating request in plan mode follows the explicit permission contract**
  - Command:
    - `cargo test -p quine-core plan_mode_write -- --nocapture`
  - Required automated test case to add/run:
    - create a plan-mode session through the normal engine/bootstrap path
    - drive one turn that issues a representative mutating or process-capable request, such as file edit, shell execution, or agent control
  - Expected result from the test:
    - if the implementation chooses `deny`, the request resolves to a deterministic permission-denied outcome, the tool does not run, and the turn completes with explicit denial text or error state
    - if the implementation chooses `ask`, the test must assert that execution pauses for approval, no mutating tool result is produced before approval, and the emitted interaction prompt is specifically the permission prompt for that blocked action
    - in either branch, the test must prove the mutating path behaves differently from the read-only scenario above
- **Integration — `EXIT_PLAN_MODE` restores normal behavior coherently**
  - Command:
    - `cargo test -p quine-core exit_plan_mode -- --nocapture`
    - `cargo test -p quine-harness exit_plan_mode -- --nocapture`
    - `cargo test -p quine-cli exit_plan_mode -- --nocapture`
  - Required automated test case to add/run:
    - create a session in plan mode
    - trigger the existing `EXIT_PLAN_MODE` path through the same channel/RPC surface used by the CLI
    - run a follow-up turn in the same session that re-attempts the same representative mutating action used in the prior plan-mode scenario
  - Expected result from the tests:
    - the session leaves plan mode exactly once
    - the runtime permission mode and persisted/runtime `plan_mode` view stay coherent after the transition
    - the follow-up mutating request no longer uses the plan-mode-specific `ask`/`deny` default and instead follows the normal non-plan permission behavior for the restored mode
- **Daemon — Multi-round local chat flow enters plan mode, allows read behavior, then exits plan mode**
  - Commands:
    - terminal 1: `cargo run --bin quine -- daemon start --socket /tmp/quine-049.sock`
    - terminal 2: `printf '/plan Inspect the repository root and tell me which top-level entries exist.\ny\n/quit\n' | cargo run --bin quine -- chat --socket /tmp/quine-049.sock`
  - Round-by-round messages sent in terminal 2:
    - round 1 message: `/plan Inspect the repository root and tell me which top-level entries exist.`
    - round 2 response to CLI confirmation prompt: `y`
    - round 3 message: `/quit`
  - Expected result from terminal 2:
    - before the assistant answer, stderr includes `Session created:`
    - the assistant completes a plan-mode answer based on repository inspection rather than claiming it edited files or executed a mutating action
    - after the assistant prints a non-empty final plan/answer, the CLI prints the exact confirmation prompt `Leave plan mode and start a normal session with this final plan? (y/n)`
    - after sending `y`, the CLI does not print `Stayed in plan mode.`
    - the session remains usable long enough to accept `/quit`, proving the exit path completed cleanly
  - Expected result from follow-up inspection:
    - `cargo run --bin quine -- log --socket /tmp/quine-049.sock --list` includes the created session
    - `cargo run --bin quine -- daemon stop --socket /tmp/quine-049.sock` exits successfully
- **Daemon — Plan mode blocks the representative mutating request until exit**
  - Commands:
    - terminal 1: `cargo run --bin quine -- daemon start --socket /tmp/quine-049-block.sock`
    - terminal 2: `printf '/plan Create ./qa-plan-mode-block.txt containing exactly blocked-in-plan-mode.\nn\n/quit\n' | cargo run --bin quine -- chat --socket /tmp/quine-049-block.sock`
  - Round-by-round messages sent in terminal 2:
    - round 1 message: `/plan Create ./qa-plan-mode-block.txt containing exactly blocked-in-plan-mode.`
    - round 2 response to CLI confirmation prompt after the assistant finishes planning: `n`
    - round 3 message: `/quit`
  - Expected result from terminal 2:
    - the assistant responds with planning text only; it does not claim the file has already been created while still in plan mode
    - the CLI prints the exact confirmation prompt `Leave plan mode and start a normal session with this final plan? (y/n)`
    - after sending `n`, the CLI prints the exact text `Stayed in plan mode.`
  - Expected result from filesystem and cleanup:
    - `test ! -f ./qa-plan-mode-block.txt` succeeds immediately after the session exits
    - `cargo run --bin quine -- daemon stop --socket /tmp/quine-049-block.sock` exits successfully
    - if the implementation chooses an `ask` prompt for the blocked mutating action inside automated tests, this daemon scenario still only requires that the file is not created during plan mode and that the user-visible plan-exit flow remains intact

## Required Evidence

- Passing `quine-core` test evidence for all three state-transition invariants:
  - first entry into `Plan` saves the original mode
  - repeated entry into `Plan` does not overwrite `pre_plan_mode`
  - exit from `Plan` restores the original mode and clears transitional state
- Passing integration-test evidence for differentiated runtime behavior:
  - one read-only tool request succeeds in a plan-mode session without spurious approval or denial
  - one representative mutating/process/agent-control request in a plan-mode session follows the exact implementation contract (`ask` or `deny`) and does not execute as a normal allowed action while the session remains in `Plan`
- Passing cross-surface evidence for `EXIT_PLAN_MODE`:
  - `quine-core`, `quine-harness`, or `quine-cli` automated tests show the existing exit path restores the prior permission mode and keeps the persisted/runtime plan-mode view coherent
- One real daemon-backed transcript that records:
  - the exact `daemon start` command
  - the exact `/plan ...` message sent through `quine chat`
  - the exact CLI confirmation prompt `Leave plan mode and start a normal session with this final plan? (y/n)`
  - one accepted exit (`y`) and one rejected exit (`n`) outcome, including the exact `Stayed in plan mode.` text for the rejected case
- Filesystem evidence for the mutating daemon scenario:
  - `./qa-plan-mode-block.txt` does not exist after the plan-mode session where the operator answered `n`
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Re-reviewed `features/plans/049-plan-mode-permission-semantics-implementation.md` before updating this QA plan.
- The implementation plan and this QA plan are now aligned on scope: explicit plan-mode transition ownership in the permission subsystem, compatibility with persisted `plan_mode`, differentiated read versus mutating behavior while in `Plan`, and reuse of the existing `EXIT_PLAN_MODE` surface.
- No new implementation-plan changes are required from QA at this revision.
