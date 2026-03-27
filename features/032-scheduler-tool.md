---
status: pending
---

# Scheduler Tool for One-Time and Recurring Fresh-Agent Launches

## Overview

Add a scheduler capability that lets an agent register future work which will create a fresh agent session with a predefined message at a scheduled time. The scheduler must support both one-time execution at a specific timestamp and recurring execution on a cadence.

This feature is intended for delayed and periodic background work such as reminders, routine repo checks, nightly planning tasks, or follow-up analyses that should run in a clean session rather than inside the current active session.

The implementation should follow the existing architecture boundaries in `CLAUDE.md`: keep cross-crate trait contracts stable, add concrete scheduling behavior behind harness-owned abstractions, and expose the feature to the agent through a tool in `quine-core`.

## Requirements

### 1. Add a dedicated scheduler tool in `quine-core`

Create a new tool file at `crates/quine-core/src/tool/scheduler.rs` and register it from `crates/quine-core/src/engine.rs` for non-plan sessions.

The tool should be named `scheduler` and should let the agent create, inspect, and cancel schedules without directly owning the timing loop itself.

The tool should expose operations at minimum:

- `create_schedule`
- `list_schedules`
- `cancel_schedule`

Suggested schema shape:

```rust
serde_json::json!({
    "type": "object",
    "properties": {
        "operation": {
            "type": "string",
            "enum": ["create_schedule", "list_schedules", "cancel_schedule"]
        },
        "message": {
            "type": "string",
            "description": "The predefined message to send to the fresh agent session when the schedule fires."
        },
        "schedule_type": {
            "type": "string",
            "enum": ["once", "recurring"]
        },
        "run_at": {
            "type": "string",
            "description": "RFC3339 timestamp for one-time schedules, e.g. 2025-01-15T18:30:00Z"
        },
        "cadence": {
            "type": "string",
            "description": "Recurring cadence expression for scheduled launches."
        },
        "system_prompt": {
            "type": "string",
            "description": "Optional system prompt override for the spawned fresh agent."
        },
        "plan_mode": {
            "type": "boolean",
            "description": "Whether the scheduled fresh agent should start in plan mode."
        },
        "auto_approve_permissions": {
            "type": "boolean",
            "description": "Whether the scheduled fresh agent session should auto-approve permission checks."
        },
        "schedule_id": {
            "type": "string",
            "description": "Identifier of an existing schedule for cancel operations."
        }
    },
    "required": ["operation"]
})
```

Behavior expectations:

- `create_schedule` returns a stable `schedule_id` plus a textual summary.
- `list_schedules` returns all active schedules with next-run information.
- `cancel_schedule` disables future launches for that schedule.
- The tool should validate arguments and return `ToolError::InvalidArguments` for malformed timestamps, missing required fields, or unsupported cadence expressions.

### 2. Fresh-agent execution semantics

When a schedule fires, the harness must create a brand-new session rather than reuse the originating session.

Required behavior:

- The scheduled run creates a fresh agent session using the same session creation path already used by `CREATE_SESSION` in `crates/quine-harness/src/server.rs` and `HarnessService::create_session` in `crates/quine-harness/src/service.rs`.
- The scheduled message is sent as the first user message to that newly created session.
- The new scheduled session must not inherit the current session's conversation history unless an explicit future enhancement adds that option.
- The fresh session may optionally use a provided `system_prompt`, `plan_mode`, and `auto_approve_permissions` payload from the schedule definition.
- The scheduler should record enough metadata to identify which parent session originally created the schedule, but the actual run executes independently.

A scheduled run should be conceptually similar to a delayed daemon-side call to:

1. `create_session(SessionConfig { ... })`
2. `send_message(new_session_id, predefined_message)`

### 3. Support both one-time and recurring schedules

The feature must support both:

- **One-time schedules**: a single execution at a specific RFC3339 timestamp.
- **Recurring schedules**: repeated executions on a cadence.

To keep the first implementation precise and testable, v1 should use exactly one canonical recurring cadence format: a fixed-interval string of the form `every:<seconds>s`.

Examples:

```json
{
  "schedule_type": "recurring",
  "cadence": "every:60s"
}
```

```json
{
  "schedule_type": "recurring",
  "cadence": "every:86400s"
}
```

Validation rules for v1:

- `cadence` must match the regex `^every:[1-9][0-9]*s$`
- the parsed seconds value must fit in `u64`
- zero-second intervals are invalid
- negative intervals and fractional intervals are invalid
- cron expressions and natural-language schedules are explicitly unsupported in v1

Suggested parser shape:

```rust
fn parse_cadence_seconds(input: &str) -> Result<u64, ToolError>;
```

Minimum recurring behavior:

- The scheduler computes `next_run_at` by adding the fixed interval to the previous scheduled fire time.
- If a run fires late because the daemon was busy, the next run should still be based on the intended cadence, not the completion time of the launched session.
- Missed runs during daemon downtime are not replayed individually in v1. On restart, recurring schedules should advance to the next future occurrence after `Utc::now()` and continue from there.
- One-time schedules whose `run_at` is already in the past when reloaded after restart should be marked as `Completed` without firing.

### 4. Add harness-owned scheduling service and persistence

The timing loop and durable schedule storage should live in `quine-harness`, not in `quine-core`.

Add a harness-internal scheduling service, for example under a new file such as:

- `crates/quine-harness/src/scheduler.rs`

This service should:

- Own the in-memory registry of schedules.
- Persist schedules to disk so they survive daemon restarts.
- Wake up at the appropriate times and launch fresh sessions.
- Mark one-time schedules as completed after they fire.
- Leave recurring schedules active until cancelled.

Suggested data model sketch:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub schedule_id: String,
    pub created_by_session_id: String,
    pub message: String,
    pub schedule_type: ScheduleType,
    pub next_run_at: chrono::DateTime<chrono::Utc>,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub system_prompt: Option<String>,
    pub plan_mode: bool,
    pub auto_approve_permissions: bool,
    pub status: ScheduleStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScheduleType {
    Once { run_at: chrono::DateTime<chrono::Utc> },
    Recurring { every_seconds: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScheduleStatus {
    Active,
    Completed,
    Cancelled,
}
```

Persistence requirements:

- Store scheduler state under a durable directory inside `~/.quine/`, similar in spirit to existing session log storage in `crates/quine-harness/src/session_log.rs`.
- Use `serde`/`serde_json` for schedule serialization.
- On daemon startup, reload persisted schedules and resume future execution.
- If persistence data is malformed, fail gracefully with clear logging rather than crashing the daemon.

### 5. Add a harness-facing interface for scheduler operations

Do not modify the core orchestration traits in `quine-core` such as `Tool` or other shared crate-boundary traits unless strictly necessary.

Instead, extend harness-side abstractions in a focused way. A suitable place is `crates/quine-harness/src/service.rs`, adding harness methods for scheduler operations. Example shape:

```rust
async fn create_schedule(&self, request: ScheduleRequest) -> Result<ScheduleHandle, HarnessError>;
async fn list_schedules(&self, created_by: Option<SessionId>) -> Result<Vec<ScheduledJob>, HarnessError>;
async fn cancel_schedule(&self, schedule_id: &str) -> Result<(), HarnessError>;
```

Then bridge the `scheduler` tool in `quine-core` to these harness capabilities through the existing execution/context plumbing in a way consistent with current tool patterns.

If additional context plumbing is needed, keep it minimal and crate-private where possible.

### 6. Define how the tool reaches the harness

The current `ExecutionContext` in `crates/quine-core/src/tool/mod.rs` already carries some core channels for tools like `spawn`. The scheduler feature should follow an equally explicit mechanism to request harness-owned scheduling work.

Acceptable implementation directions:

- Extend the existing core/harness message path with explicit scheduler requests and replies.
- Or provide a crate-private scheduling handle into execution context during session setup.

The feature must document one chosen path and keep the boundary understandable.

Non-goal for v1: do not build scheduler logic by spawning a long-lived agent session that sleeps with `bash` or `tokio::time` inside the core event loop.

### 7. Surface schedule activity in logs and notifications

Scheduled launches should be auditable.

Minimum requirements:

- Log schedule creation, cancellation, trigger, and launch result through harness logging, reusing existing patterns in `crates/quine-harness/src/server.rs` and `crates/quine-harness/src/session_log.rs` where appropriate.
- Record which schedule launched which session.
- Ensure failures to create or start a scheduled session are visible in logs.

Optional but recommended for v1 if low-cost:

- Emit a harness notification/event when a schedule fires and a child/fresh session is launched.

This should not block the initial implementation if logging alone gives sufficient observability.

### 8. Keep plan mode restrictions intact

Plan-mode sessions should continue to expose only their restricted tool set. The new `scheduler` tool should therefore be registered only for non-plan sessions in `crates/quine-core/src/engine.rs`, unless there is an explicit product decision to allow scheduling from plan mode.

For the initial implementation, the feature should require:

- `scheduler` is available in normal sessions.
- `scheduler` is not available in plan-mode sessions.

### 9. CLI inspection support is optional for v1

The initial user-facing API may be tool-driven only. It is acceptable to defer dedicated CLI subcommands such as `quine schedule list` if the agent can manage schedules through the tool.

If CLI support is added, keep it focused and consistent with existing command structure in `crates/quine-cli/src/main.rs`, but this is not required for the first implementation.

## Acceptance Criteria

- `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check` pass.
- A new `scheduler` tool exists and is registered in normal sessions.
- The tool can create a one-time schedule with an RFC3339 timestamp and returns a stable `schedule_id`.
- The tool can create a recurring schedule using only the `every:<seconds>s` cadence format and returns a stable `schedule_id`.
- Invalid cadence strings such as `cron:* * * * *`, `every:0s`, `every:-5s`, and `every:1.5s` are rejected.
- The tool can list active schedules with enough information to identify next run time and configuration.
- The tool can cancel a schedule by `schedule_id`.
- When a one-time schedule fires, the harness creates a fresh session and sends the predefined message as that session's first user message.
- When a recurring schedule fires, the harness creates a fresh session and reschedules the next run according to the fixed interval.
- Schedule state persists across daemon restarts.
- One-time schedules are not re-run after successful completion.
- Past-due one-time schedules reloaded after restart are marked completed without executing.
- Recurring schedules reloaded after restart resume at the next future occurrence and do not replay missed runs.
- Cancelled schedules do not trigger again after restart.
- Plan-mode sessions do not expose the `scheduler` tool.
- Existing session spawning, messaging, and permission flows continue to work unchanged.

Specific tests required:

- Unit tests in `crates/quine-core/src/tool/scheduler.rs` for:
  - argument validation
  - timestamp parsing
  - cadence parsing/validation for `every:<seconds>s`
  - rejection of cron-like and natural-language cadence strings
- Unit tests in `crates/quine-harness/src/scheduler.rs` for:
  - one-time trigger lifecycle
  - recurring next-run computation
  - cancellation
  - persistence load/save round-trip
  - restart behavior with pending future schedules
- Integration tests in `crates/quine-harness/tests/` or an appropriate crate for:
  - creating a schedule then observing a fresh launched session
  - recurring schedule creating multiple fresh sessions over controlled test time
  - cancelled schedule never firing
- Existing tests must continue to pass.

## QA Test Cases (add to `qa/test_cases.json`)

```json
[
  {
    "name": "scheduler_once_launches_fresh_agent",
    "description": "A one-time schedule launches a fresh agent session with the predefined message",
    "steps": [
      "Start the daemon",
      "Create a one-time schedule a short time in the future using the scheduler tool",
      "Wait until the scheduled time passes",
      "Verify a new session is created",
      "Verify the new session's first user message matches the scheduled message"
    ]
  },
  {
    "name": "scheduler_recurring_launches_repeatedly",
    "description": "A recurring cadence creates multiple fresh agent sessions over time",
    "steps": [
      "Start the daemon",
      "Create a recurring schedule using a fixed-interval cadence like `every:60s`",
      "Wait long enough for at least two triggers",
      "Verify multiple fresh sessions are launched from the same schedule",
      "Verify each launched session starts with the configured message"
    ]
  },
  {
    "name": "scheduler_cancel_prevents_future_runs",
    "description": "Cancelling a schedule stops future launches",
    "steps": [
      "Create a recurring schedule",
      "Verify the first run occurs",
      "Cancel the schedule",
      "Wait through another expected trigger window",
      "Verify no additional session is launched"
    ]
  },
  {
    "name": "scheduler_persists_across_restart",
    "description": "Schedules survive daemon restart and continue future execution",
    "steps": [
      "Create a future recurring or one-time schedule",
      "Restart the daemon before the next trigger",
      "Verify the schedule is reloaded",
      "Verify the future run still occurs according to the documented missed-run policy"
    ]
  }
]
```

## Non-Goals (Deferred)

- Editing an existing schedule in place
- Natural-language schedule parsing such as "next Tuesday at 4pm"
- Catch-up replay of all missed recurring runs during downtime
- Per-schedule workspace overrides beyond existing session config fields
- Cron expressions or other calendar-based recurring syntax
- Complex calendar semantics beyond the chosen canonical cadence format
- Dedicated CLI management commands if the tool interface is sufficient for v1
- Chaining schedule completion into parent-session callbacks or IPC replies
