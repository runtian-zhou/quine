---
status: pending
---

# Scheduled Agent Runs

## Overview

Add a scheduler feature that can launch an agent with a predefined message either:

- once at a future time
- repeatedly on a configured cadence

The scheduler should belong to `quine-core`, because it is a first-class agent-orchestration capability rather than only a daemon transport concern. The harness and CLI should expose and persist scheduler operations, but the schedule model and execution logic should be defined in core-facing types and flows.

The goal is to support automation workflows such as:

- "At 2026-04-01T09:00:00-04:00, spawn an agent that summarizes yesterday's logs."
- "Every 6 hours, spawn an agent that audits pending feature requests."
- "Every weekday morning, spawn an agent that posts a build-health summary."

## Requirements

### 1. Add scheduler-managed job definitions

Introduce a core-owned scheduled-job model that records:

- stable job identifier
- target message to send to the spawned agent
- schedule kind: one-off or recurring
- schedule parameters
- optional system prompt override
- optional working directory
- optional skills list
- optional `plan_mode`
- optional `auto_approve_permissions`
- job enabled/disabled state
- last run metadata
- next run timestamp

The model should live in `quine-core` and remain crate-private where possible, with minimal public re-exports.

### 2. Support one-off and recurring schedules

The first implementation must support:

- one-off execution at an explicit future timestamp
- recurring execution on a predefined cadence

Recurring cadence should be explicit and structured, not a free-form natural-language string. The implementation may choose a constrained interval/cron representation, but it must be deterministic, serializable, and testable.

Non-goal for the first version: full natural-language scheduling.

### 3. Execute jobs by spawning fresh agent sessions

When a scheduled job fires, the scheduler should create a fresh session and submit the configured message as the first user message. The scheduled run should not depend on an already-open client session.

The spawned run should:

- use the same session creation path as normal core-managed sessions exposed through the harness
- produce normal session logs in `.quine/logs`
- emit enough metadata to correlate the spawned session back to the scheduled job

The scheduler must not bypass the core engine or invent a parallel agent loop.

### 4. Add scheduler lifecycle management in core

`quine-core` should own the scheduler logic that:

- tracks pending jobs
- wakes when the next job becomes due
- executes due jobs without blocking unrelated IPC requests
- recomputes next-run time for recurring jobs
- marks one-off jobs complete after a successful launch

The design should make clear how the scheduler is initialized by the harness daemon at startup and how it behaves if the daemon restarts while jobs exist.

### 5. Persist scheduled jobs across daemon restarts

Scheduled jobs must survive harness restart.

Add a small persisted job store under Quine-managed local state, for example under `.quine/`. The persisted representation should be JSON/serde-based and robust against partial corruption:

- malformed job entries should not crash daemon startup
- startup should surface clear errors for invalid records
- valid jobs should continue loading even if one record is bad

### 6. Add CLI surface for managing schedules

Add CLI commands to:

- create a scheduled job
- list scheduled jobs
- show a single scheduled job
- disable or enable a scheduled job
- delete a scheduled job

The create command must support both:

- one-off future execution
- recurring cadence configuration

The initial UX may be subcommand- and flag-based rather than interactive.

### 7. Expose scheduler operations through harness IPC

Extend the harness protocol and service layer with explicit scheduler operations so the CLI can manage jobs through the daemon. The harness should adapt to core-owned scheduler logic rather than redefining schedule semantics itself.

### 8. Add focused tests

Add automated coverage for:

- one-off job scheduling and execution
- recurring cadence next-run computation
- persistence roundtrip for scheduled jobs
- harness startup with stored jobs backed by core-owned schedule logic
- enable/disable/delete flows
- CLI request parsing for schedule management commands
- job execution producing a real spawned session and session log

Time-sensitive tests should avoid flaky wall-clock sleeps where possible; prefer injectable clocks or deterministic scheduling seams if the implementation introduces them.

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Users can register a one-off future job that launches a fresh agent session with a configured message.
- Users can register a recurring job with a predefined cadence that launches fresh agent sessions on schedule.
- Scheduled jobs persist across daemon restarts.
- There are CLI commands to create, inspect, list, enable/disable, and delete scheduled jobs.
- Scheduled runs are visible through normal session logs and can be correlated back to the originating job.

## Non-Goals (Deferred)

- Natural-language schedule parsing
- Remote/distributed scheduler coordination
- Editing an existing job in place
- Delivery guarantees stronger than "best effort once the local daemon is running"
- Full cron-expression compatibility if a narrower recurring representation is sufficient for the first version
