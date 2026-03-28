# Spawn and Subagent QA Smoke Coverage — QA Plan

Feature request: `features/032-spawn-subagent-qa-smoke.md`

Summary: Verify the project can clearly demonstrate the intended behavioral split between `spawn` and `subagent` using small, repeatable checks.

## Open Questions

- What is the preferred permanent location for a lightweight QA scenario document in this repository?
- Should QA require a harness-level end-to-end scenario for `spawn`, or is unit-level evidence acceptable for this narrow smoke feature?

## Agreement Status

Status: agreed

QA review confirms the implementation plan is appropriately scoped and testable. Both plans agree to validate the existing `spawn`/`subagent` split through targeted `quine-core` tests and lightweight QA guidance, while avoiding architectural changes.

## Test Strategy

- Require deterministic unit coverage for `SpawnTool` because its primary contract is argument parsing plus dispatch to `core_input`.
- Treat existing `subagent` tests as primary evidence for inline delegation behavior, adding only the smallest missing case if needed.
- Add a brief manual or documented smoke scenario that demonstrates the user-visible distinction between `spawn` and `subagent`.

## Scenarios

- Invoke `spawn` with a valid `core_input` test harness and verify the returned payload contains a child session identifier.
- Invoke `spawn` without `core_input` and verify the tool returns an internal error describing the missing channel.
- Invoke `subagent` for a simple delegated task and verify it returns the final result directly.
- Review the QA artifact and confirm it explains when to expect an immediate result versus a child session handle.

## Required Evidence

- Targeted unit test output for `spawn` coverage.
- Existing or updated `subagent` unit test output proving inline completion behavior.
- Full workspace `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check` results.

## Implementation Feedback

Implementation plan reviewed and accepted. The proposed file touch points and validation steps are compatible with the QA strategy, and the scope stays focused on tests plus QA documentation rather than behavior changes.
