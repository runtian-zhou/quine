# 046 Interactive Approval Routing — QA Plan

Short summary: Verify Quine Feature 4 interactive approval routing, including `ask`-driven pause/resume behavior, daemon approval transport, operator approve/deny responses, and deterministic failure handling for unresolved approvals.

## Open Questions

- None. This plan stays scoped to Feature 4 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest `features/plans/046-interactive-approval-routing-implementation.md` revision after expanding this QA plan with concrete daemon workflows. The implementation scope, optional session-rule behavior, and fail-safe unresolved-approval handling are aligned, and this QA plan has no remaining open questions.

## Test Strategy

- Cover the workflow at three layers:
  - `quine-core` integration tests for paused execution and resumed completion
  - `quine-harness` daemon tests for approval request/response plumbing
  - `quine-cli` interactive rendering or operator-response tests where feasible
- Because this slice affects `quine-core`, include at least one concrete multi-round local-daemon scenario with exact messages and expected behavior.

## Scenarios

- **Integration — Pause on `ask`**
  - Add a `quine-core` integration test that configures the permission evaluator to return `ask` for a single write-capable tool invocation.
  - Drive one agent turn until the tool reaches the permission gate.
  - Expect, before any response is submitted:
    - one emitted interaction request for the paused tool path
    - pending approval state recorded in session context
    - no tool completion result and no final assistant turn completion yet
  - Assert the interaction payload includes a stable prompt plus enough correlation metadata for a later response to resume the same paused request.
- **Integration — Resume on Approve**
  - In the same test harness style, submit the approval response that represents `approve once` after the pending request is emitted.
  - Expect:
    - the paused tool execution resumes exactly once
    - the original tool result is delivered to the agent loop
    - the turn reaches normal completion
    - pending approval state is cleared after completion
- **Integration — Deny Once**
  - Submit the denial response that represents `deny once` for a paused request.
  - Expect:
    - the tool path resolves to the implementation's chosen deterministic denied outcome (`PermissionDenied` or mapped cancellation)
    - the assistant turn completes with a non-success outcome instead of hanging
    - pending approval state is cleared
    - no follow-up tool execution occurs after denial
- **Integration — Approve and Add Session Rule**
  - Only run this scenario if Feature 046 includes the optional session-scoped rule-writing behavior described in the implementation plan.
  - First turn: trigger the same `ask`-gated tool call and answer with the operator response that means approve plus remember for this session.
  - Second turn in the same session: trigger the equivalent tool call again.
  - Expect:
    - the first turn pauses, receives the remembered-approve response, resumes, and completes successfully
    - the second turn executes without emitting another approval prompt for the same effective permission decision
    - the remembered allowance is session-scoped only
- **Daemon — Approve Path via one-off CLI**
  - Start the daemon explicitly in one terminal:
    - `cargo run --bin quine -- daemon start --socket /tmp/quine-046.sock`
  - In a second terminal, start a one-off run that should trigger an `ask`-gated write-capable tool call after implementation lands:
    - `cargo run --bin quine -- run --json --socket /tmp/quine-046.sock "Use the minimal file-editing tool needed to create ./qa-approval-check.txt containing exactly approved-by-operator."`
  - Expected result from the first command before responding:
    - stdout is JSON with `"interaction_needed": true`
    - the JSON includes the current `session_id`
    - the JSON includes a non-empty `prompt`
    - if the implementation adds prompt discrimination, `source_label` clearly identifies the permission approval source rather than a generic tool question
    - `tool_calls` includes the write-capable tool being paused rather than a completed success result
    - there is no final success text claiming the file was created yet
  - Respond in the second terminal using the returned session ID and the exact approval option text implemented for one-time approval:
    - `cargo run --bin quine -- respond --json --socket /tmp/quine-046.sock --session <SESSION_ID> "approve once"`
  - Expected result from the response command:
    - stdout is JSON with the same `session_id`
    - `response` reports successful completion of the user request after approval
    - `tool_calls` still show the gated write-capable tool invocation
    - the command exits successfully
    - `./qa-approval-check.txt` now exists in the workspace with exact contents `approved-by-operator`
  - Cleanup after the scenario:
    - `rm -f ./qa-approval-check.txt`
    - `cargo run --bin quine -- daemon stop --socket /tmp/quine-046.sock`
- **Daemon — Deny Path via one-off CLI**
  - Start the daemon:
    - `cargo run --bin quine -- daemon start --socket /tmp/quine-046.sock`
  - Trigger the same `ask`-gated write request:
    - `cargo run --bin quine -- run --json --socket /tmp/quine-046.sock "Use the minimal file-editing tool needed to create ./qa-deny-check.txt containing exactly should-not-exist."`
  - Expected pre-response output:
    - stdout is JSON with `"interaction_needed": true`
    - the prompt is the permission approval prompt for the paused write attempt
    - no file is created yet
  - Submit the exact deny option text implemented for one-time denial:
    - `cargo run --bin quine -- respond --json --socket /tmp/quine-046.sock --session <SESSION_ID> "deny once"`
  - Expected result after denial:
    - the command either exits non-zero with a deterministic denial error, or exits zero with JSON/text that clearly states the request was denied; whichever contract implementation chooses must be asserted consistently in tests
    - no `qa-deny-check.txt` file exists
    - no second approval prompt is emitted for the same paused request
    - the session is no longer wedged waiting for an interaction response
  - Cleanup:
    - `rm -f ./qa-deny-check.txt`
    - `cargo run --bin quine -- daemon stop --socket /tmp/quine-046.sock`
- **Daemon — Missing responder or timeout fails safe**
  - Start the daemon:
    - `cargo run --bin quine -- daemon start --socket /tmp/quine-046.sock`
  - Trigger the same `ask`-gated write request and capture the returned `session_id`:
    - `cargo run --bin quine -- run --json --socket /tmp/quine-046.sock "Use the minimal file-editing tool needed to create ./qa-timeout-check.txt containing exactly timeout-test."`
  - Do not submit any `quine respond` command.
  - Expected behavior to verify with daemon/integration tests and any available session-log inspection:
    - the session does not implicitly approve the action
    - the write target file is never created
    - the pending interaction eventually resolves to the implementation's deterministic non-success outcome for unresolved approvals (timeout denial, cancellation, or equivalent explicit failure)
    - the session can accept later work or terminate cleanly; it does not remain permanently stuck in a hidden paused state
  - If the implementation exposes the outcome through logs or a follow-up CLI command, capture that exact evidence; otherwise this scenario must be covered by a deterministic automated test in `quine-core` or `quine-harness` that asserts the timeout result directly.
  - Cleanup:
    - `rm -f ./qa-timeout-check.txt`
    - `cargo run --bin quine -- daemon stop --socket /tmp/quine-046.sock`
- **Daemon — Interactive chat multi-round flow**
  - Run an end-to-end REPL check in a real terminal because Feature 046 also changes interactive operator routing:
    - `cargo run --bin quine -- chat --socket /tmp/quine-046.sock`
  - Round 1 user message:
    - `Use the minimal file-editing tool needed to create ./qa-chat-check.txt containing exactly chat-approved.`
  - Expected interactive behavior before operator input:
    - the chat UI presents a permission-approval prompt that is visually distinct from a generic `ask_user` prompt if the implementation adds that distinction
    - the assistant does not claim completion before the approval choice is made
  - Operator response in the same chat session:
    - choose the one-time approval option, or type the exact response text if the UI remains free-form
  - Expected final result:
    - the agent completes the request successfully after approval
    - `./qa-chat-check.txt` exists with exact contents `chat-approved`
    - there is no duplicate or stale pending approval prompt left in the UI after completion
  - Cleanup:
    - delete `./qa-chat-check.txt`
    - exit chat and stop the daemon

## Required Evidence

- Passing `quine-core` automated coverage for:
  - pause-on-`ask`
  - resume-on-approve
  - deterministic deny outcome
  - deterministic timeout or missing-responder outcome
- Passing `quine-harness` transport coverage showing `INTERACTION_NEEDED` emission plus `SUBMIT_INTERACTION_RESPONSE` resume/deny routing.
- Passing `quine-cli` coverage for one-off and/or REPL interaction rendering, including permission-prompt discrimination if that fielding is added.
- One machine-verifiable approve transcript from the daemon scenario above that records:
  - the exact `cargo run --bin quine -- daemon start --socket /tmp/quine-046.sock` command
  - the exact `cargo run --bin quine -- run --json ...` invocation and returned `session_id`
  - the exact `cargo run --bin quine -- respond --json ... "approve once"` invocation
  - the final JSON output plus file-system evidence that `./qa-approval-check.txt` was created only after approval
- One machine-verifiable deny transcript that records:
  - the exact `run --json` invocation
  - the exact `respond --json ... "deny once"` invocation
  - the final deterministic denial output or error contract
  - file-system evidence that `./qa-deny-check.txt` was not created
- Evidence for unresolved approval handling, either:
  - a daemon/session-log transcript that shows the timeout or unreachable-responder result explicitly, or
  - an automated test result that asserts the exact timeout-resolution contract directly
- If optional session-rule persistence ships in this slice, evidence that the remembered approval suppresses the second prompt only within the same session.
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Re-reviewed `features/plans/046-interactive-approval-routing-implementation.md` before marking this QA plan agreed.
- The implementation plan is aligned with this QA plan's scope: reuse of the existing interaction transport, `quine-core` ownership of pause/resume correlation, CLI rendering updates, deterministic unresolved-approval failure, and optional session-rule persistence only if it fits cleanly in this slice.
- This QA plan now supplies the concrete daemon commands, round-by-round prompts, operator responses, and expected outputs required by `.claude/commands/feature-planning.md`.
- No further implementation-plan changes are required from QA review at this revision. The remaining agreement update is for the implementation doc to re-review this latest QA revision and mark its own `## Agreement Status` accordingly.
