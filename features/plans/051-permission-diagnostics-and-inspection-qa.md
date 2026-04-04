# 051 Permission Diagnostics and Inspection — QA Plan

Short summary: Verify Quine Feature 9 permission diagnostics and inspection, including structured decision explanations, operator-visible runtime permission state, pending-approval visibility, and actionable reasons for denied or prompted requests.

## Open Questions

- None. This plan stays scoped to Feature 9 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest `features/plans/051-permission-diagnostics-and-inspection-implementation.md` revision and aligned this QA plan to its existing `GET_SESSION_CONTEXT` and `/context` approach. Both docs now describe the same concrete inspection scenarios and have no unresolved open questions.

## Test Strategy

- Test structured permission explanations close to `quine-core` outcome types.
- Add integration coverage for inspection surfaces exposed through existing harness and CLI context flows.
- Prefer current `GET_SESSION_CONTEXT` and `/context` surfaces rather than inventing a QA-only diagnostics command.
- Require at least one concrete local-daemon inspection flow because this feature changes user-visible runtime state.

## Scenarios

- **Unit — Outcome Explanation Serialization**
  - **Command**: `cargo test -p quine-core permission:: -- --nocapture`
  - **Expected added coverage**: targeted tests in `crates/quine-core/src/permission/outcome.rs` or the colocated permission module asserting representative `allow`, `deny`, and `ask` outcomes serialize with stable `source`, `reason`, and any matched-rule metadata.
  - **Representative fixtures**:
    - an allow outcome sourced from a built-in rule or mode default
    - a deny outcome sourced from a session or project rule with a human-readable reason
    - an approval-required outcome carrying an approval summary
  - **Expected result**: the test output passes and the assertions prove the serialized shape preserves enough machine-readable data for harness snapshots and CLI rendering to report why the decision happened.

- **Integration — Inspection Reflects Runtime Mode, Rule Sources, and Additional Roots**
  - **Command**: `cargo test -p quine-harness get_session_context -- --nocapture`
  - **Expected added coverage**: an integration or service-level test that creates a session with known permission state, calls the existing `GET_SESSION_CONTEXT` path, and verifies the returned snapshot includes current permission mode, rule partitions by source, prompt behavior, and additional allowed roots.
  - **Expected result**: the harness test exits successfully and proves the snapshot is sourced from live runtime permission state rather than a separately invented debug structure.

- **Integration — Pending Approval Visibility Appears Through Session Context**
  - **Command**: `cargo test -p quine-core approval -- --nocapture`
  - **Expected added coverage**: a core or harness test that drives a permission outcome requiring approval, attaches pending approval state to the session, and verifies the same snapshot path now includes a pending approval summary with request identity and request reason.
  - **Expected result**: the test proves pending approval is visible through the existing inspection surface before the operator responds, and clears or updates deterministically once the request completes.

- **Integration — Last Denial / Prompt Reason Is Surfaced Actionably**
  - **Command**: `cargo test -p quine-core permission:: -- --nocapture`
  - **Expected added coverage**: a session-level test that triggers a denied request and a prompted request, then inspects the stored diagnostic summary exposed via session context.
  - **Expected result**: the surfaced explanation includes an actionable reason string plus a structured source such as mode default, rule source, or sandbox/headless policy; the values are specific enough for an operator to understand why the action did not run.

- **Real daemon multi-round scenario — `/context` shows permission state before and after a denied request**
  - **Daemon start command**: `cargo run --bin quine -- daemon start --socket /tmp/quine-feature-051.sock`
  - **Chat command**: `cargo run --bin quine -- chat --socket /tmp/quine-feature-051.sock`
  - **Round 1 user message**: `/context`
  - **Expected round 1 result**: the rendered session context includes the current session ID, current plan/permission mode information, and a permission diagnostics section or JSON fields showing at least current mode and prompt behavior, even before any denied action occurs.
  - **Round 2 user message**: `Attempt an action that should be denied by the current permission policy, and then explain the denial.`
  - **Expected round 2 result**: the assistant reports a permission denial or approval-required block rather than claiming success, and the underlying session state records a last permission decision summary.
  - **Round 3 user message**: `/context`
  - **Expected round 3 result**: the rendered session context now includes the last permission decision summary with an actionable reason and source attribution; if a rule matched, the output includes that rule source, and if the deny came from mode or headless policy, that source is named explicitly.
  - **Cleanup command**: `cargo run --bin quine -- daemon stop --socket /tmp/quine-feature-051.sock`

- **Real daemon scenario — pending approval appears in inspection while interactive approval is outstanding**
  - **Daemon start command**: `cargo run --bin quine -- daemon start --socket /tmp/quine-feature-051.sock`
  - **Chat command**: `cargo run --bin quine -- chat --socket /tmp/quine-feature-051.sock`
  - **Round 1 user message**: `Attempt an action that requires approval and wait for operator input instead of silently skipping it.`
  - **Expected round 1 result**: the session surfaces an approval interaction and remains blocked on operator input rather than failing headlessly.
  - **Inspection command while approval is pending**: `cargo run --bin quine -- ps --json --socket /tmp/quine-feature-051.sock` followed by `cargo run --bin quine -- chat --socket /tmp/quine-feature-051.sock --resume <SESSION_ID>` and `/context`
  - **Expected inspection result**: the inspected session state shows a pending approval summary containing the outstanding request ID or equivalent summary plus the request reason/source details.
  - **Approval response command**: `cargo run --bin quine -- respond --socket /tmp/quine-feature-051.sock --session <SESSION_ID> "deny"`
  - **Expected post-response result**: the pending approval summary clears, and `/context` now shows the latest denial summary instead.
  - **Cleanup command**: `cargo run --bin quine -- daemon stop --socket /tmp/quine-feature-051.sock`

## Required Evidence

- Output from `cargo test -p quine-core permission:: -- --nocapture` showing stable explanation serialization and last-decision coverage.
- Output from `cargo test -p quine-harness get_session_context -- --nocapture` showing permission snapshot fields in the existing context call.
- Output from the targeted approval/pending-state test showing pending approval becomes inspectable through the same snapshot path.
- A daemon-backed transcript or captured output showing:
  - the exact `/context` command before any denied action
  - the denied or approval-required action
  - the follow-up `/context` output with last permission decision source/reason fields
- A daemon-backed transcript or captured output showing pending approval visibility before operator response and cleared state after `respond`.
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Re-reviewed `features/plans/051-permission-diagnostics-and-inspection-implementation.md` before updating this QA plan.
- The implementation plan and this QA plan are aligned on extending existing `GET_SESSION_CONTEXT` and `/context` surfaces, with no new diagnostics RPC or management UI.
- This QA revision adds the concrete executable detail previously missing: exact test commands, exact daemon/CLI inspection flows, exact prompts, and explicit expected inspection content for current mode, rule sources, pending approvals, and last decision reasons.
- No further implementation-plan changes are requested from QA at this revision.
