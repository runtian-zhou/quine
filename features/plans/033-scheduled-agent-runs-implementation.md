# 033 Scheduled Agent Runs — Implementation Plan

## Ownership

- Owner: implementor agent
- Status: draft

## Goal

Add core-owned scheduled jobs that can create fresh agent sessions with a predefined message either once at a future time or repeatedly on a structured cadence, while preserving the existing crate-boundary contracts and session execution path.

## Proposed Approach

1. Add a crate-private scheduler domain in `quine-core` with serializable job definitions, next-run computation, and execution hooks for spawning fresh sessions.
2. Have the harness daemon host and persist that core-owned scheduler, but keep schedule semantics and orchestration logic out of `quine-harness`.
3. Reuse the existing session creation plus first-message dispatch path so scheduled runs behave like normal core-managed sessions and continue writing standard session logs.
4. Extend the harness service plus JSON-RPC protocol with explicit schedule-management operations that delegate to the core-owned scheduler.
5. Keep cadence parsing structured and deterministic in the first version. A constrained recurring representation is preferable to free-form strings.

## Files To Inspect

- `crates/quine-core/src/channel.rs`
- `crates/quine-core/src/engine.rs`
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
- `crates/quine-cli/src/client.rs`

## Concrete Implementation Notes

- The feature should live primarily in `quine-core`, likely as a new scheduler-focused module plus small engine/channel integration points. `quine-harness` should host persistence and IPC adaptation, but it should not define schedule semantics on its own.
- The cleanest execution seam is likely a core-owned scheduler job that resolves into the same session-start plus first-message path already used by normal runs, rather than a harness-only wrapper that treats scheduling as transport state.
- Persistence still likely belongs beside other harness-managed local state, not inside session logs. The job store should be separate from `.quine/logs` so log retention and scheduler persistence remain independent concerns.
- Recurring cadence should start with a narrow explicit representation. Candidate shapes:
  - fixed interval, such as every `N` minutes/hours/days
  - calendar-based cadence, such as daily at a local time or weekly on named weekdays
  - a constrained cron-like form if existing dependencies and testability stay reasonable
- The implementation should store `next_run_at` explicitly so daemon startup can resume scheduling without recomputing from an ambiguous "last run only" model.
- The job-execution path should write correlation metadata that lets a user map a scheduled job to the resulting session or sessions. This likely needs a small core-owned metadata seam that the harness can surface in logs or job status views.
- Disabled jobs should stay persisted but never be selected for execution until re-enabled.
- One-off jobs should transition to a terminal state after launch or retain enough metadata to show that they have already fired.

## Open Questions

- The first version should decide whether recurring cadence is interval-based only or also supports calendar-style recurrence. Interval-only is simpler but less expressive for "every weekday at 09:00".
- Time zone semantics must be explicit. The safest initial model is to persist timestamps in UTC and only accept local-time recurrence when the job representation also records a time zone or offset.
- The feature should define whether missed runs after daemon downtime are skipped or backfilled. The likely first-version behavior is "skip past-due occurrences and schedule the next valid run" unless the product requirement explicitly needs catch-up behavior.

## Review Of QA Plan

- QA should emphasize deterministic schedule computation and startup recovery, not just CLI parsing.
- Time-based tests should avoid long sleeps. The implementation should expose at least one seam for deterministic next-run calculation or scheduler wakeup decisions.
- The most important integration proof is that a fired job produces a real session and a normal session log, rather than only mutating scheduler metadata.
- Persistence coverage should include partially invalid store contents so scheduler startup remains resilient.
- Because the scheduler is core-owned, QA should also validate that the harness remains a thin adapter for persistence and IPC rather than becoming the semantic source of truth.

## Agreement Log

- Initial draft created from repo inspection. No implementor/QA agent cross-review has been performed yet in this session.
