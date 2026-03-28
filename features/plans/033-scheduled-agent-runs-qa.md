# 033 Scheduled Agent Runs — QA Plan

## Ownership

- Owner: QA agent
- Status: draft

## Test Objectives

Verify that scheduled jobs are owned by `quine-core`, hosted and persisted by the harness daemon, launch fresh agent sessions with the configured message on both one-off and recurring schedules, and remain operable through a CLI management surface without regressing normal session behavior.

## Planned Coverage

1. Job-definition validation for one-off and recurring schedules.
2. Next-run computation for recurring cadence.
3. Persistence roundtrip and startup recovery with stored jobs.
4. Execution of due jobs into fresh agent sessions through the core-owned scheduler path.
5. CLI coverage for create/list/show/enable/disable/delete flows.
6. Correlation evidence showing scheduled jobs produce normal session logs.
7. Workspace validation via `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check`.

## Concrete QA Strategy

### 1. Schedule model and parsing

- Add unit coverage for schedule parsing/validation at the smallest deterministic seam.
- Required cases:
  - valid one-off future timestamp
  - invalid or past one-off timestamp rejection if the implementation chooses to reject past times at creation
  - valid recurring cadence
  - invalid cadence rejection

### 2. Next-run computation

- Add deterministic tests for cadence advancement without relying on real wall-clock sleeping.
- Required cases:
  - one-off run has no next occurrence after launch
  - recurring cadence computes the expected next run after each execution
  - disabled jobs are excluded from due-job selection

### 3. Persistence and restart behavior

- Add tests that write jobs to the persisted store and reload them on simulated daemon startup.
- Required cases:
  - roundtrip preserves schedule fields and enabled state
  - malformed stored entry does not crash startup
  - valid entries still load when one stored record is bad

### 4. Job execution path

- QA expects an integration seam proving that a due job creates a fresh session and submits the configured message through the normal core execution path surfaced by the harness.
- Required evidence:
  - a session is created when a due job fires
  - the configured message becomes the first user message
  - the resulting run is visible through normal session logs
  - the log or job metadata contains enough information to correlate the run to the originating job

### 5. CLI management surface

- Add focused CLI tests for:
  - create one-off job
  - create recurring job
  - list jobs
  - show one job
  - disable and re-enable a job
  - delete a job
- Unknown or invalid flag combinations should fail locally with a clear error.

## Risk Notes

- **Ownership risk**: If schedule semantics are split ambiguously between `quine-core` and `quine-harness`, persistence and execution can drift or behave inconsistently.
- **Daemon hosting risk**: If scheduling logic leaks into the CLI, jobs will stop working when no client is attached.
- **Time semantics risk**: Local time, UTC, and cadence advancement can drift or become ambiguous if the representation is under-specified.
- **Persistence risk**: A corrupted store could block all scheduler startup if not isolated carefully.
- **Execution-path risk**: Scheduled jobs might accidentally bypass normal session creation, leaving no standard logs or session metadata.
- **Catch-up risk**: Restart behavior for missed runs can surprise users if the skip/backfill policy is not explicit and tested.

## Validation Targets

- `crates/quine-core/src/session.rs`
- `crates/quine-core/src/lib.rs`
- `crates/quine-harness/src/service.rs`
- `crates/quine-harness/src/local.rs`
- `crates/quine-harness/src/server.rs`
- `crates/quine-harness/src/protocol.rs`
- `crates/quine-harness/src/config.rs`
- `crates/quine-harness/src/session_log.rs`
- `crates/quine-cli/src/main.rs`
- `crates/quine-cli/src/agent_ctl.rs`

## Suggested Manual Scenarios

- Create a one-off job for a near-future timestamp and confirm it launches a fresh session with the configured message.
- Create a recurring job with a short cadence in a test environment and confirm repeated launches appear in session logs.
- Restart the daemon after creating jobs and confirm they are still listed and still fire.
- Disable a recurring job and confirm no new runs occur until re-enabled.
- Delete a job and confirm it disappears from the list and no longer schedules runs.

## Required Evidence

- Deterministic unit tests for schedule parsing and next-run computation.
- Persistence tests covering restart and malformed-entry handling.
- Integration proof that a due job produces a real session and a real session log entry.
- CLI tests for create/list/show/enable/disable/delete operations.
- Passing workspace checks: `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check`.

## Open Questions

- The implementation should clarify whether missed runs during daemon downtime are skipped or replayed. QA can support either policy, but it must be explicit and tested.
- The implementation should clarify whether recurring cadence is interval-only in the first version or also supports calendar-based recurrence.

## Review Of Implementation Plan

- The implementation direction should be updated so the scheduler is core-owned and the harness remains an adapter for hosting, persistence, and IPC.
- QA agrees that cadence should be structured and deterministic rather than natural-language.
- QA wants explicit test seams for time computation and persistence so the feature does not rely on long sleeps or fragile wall-clock assumptions.

## Agreement Log

- Initial draft created from repo inspection. No implementor/QA agent cross-review has been performed yet in this session.
