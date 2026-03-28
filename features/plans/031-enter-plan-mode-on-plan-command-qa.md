# Enter Plan Mode on `/plan` Command — QA Plan

Feature request: `features/031-enter-plan-mode-on-plan-command.md`

Summary: Validate that entering `/plan` from interactive chat surfaces starts a plan-mode workflow locally, preserves existing command behavior, and does not regress normal chat interactions.

## Open Questions

- What exact user-visible confirmation should appear when `/plan` creates a replacement session in the REPL and TUI?
- Is the accepted source of planning prompt text the existing `feature-request` legacy command file, or should QA expect a new dedicated `/plan` command file?

## Agreement Status

Status: pending

## Test Strategy

- Prefer deterministic unit coverage for parsing and local command routing in `quine-cli`.
- Add at least one integration-style test or high-fidelity unit around session creation payloads to prove `plan_mode: true` is sent for `/plan`.
- Manually verify both REPL and TUI flows because they maintain independent input loops.

## Scenarios

- Enter `/plan add a grep tool` in REPL and verify the first outbound message is `add a grep tool` and the created session is plan-mode.
- Enter `/plan` in REPL and verify no empty `SEND_MESSAGE` request is issued and the user is prompted for more detail.
- Enter `/plan review the TUI` in TUI and verify the UI reflects plan mode after the command is processed.
- Enter `/does-not-exist` in both interfaces and verify the command is rejected locally.
- Enter plain text and `/quit` in REPL to confirm old behavior is preserved.

## Required Evidence

- Unit test output covering slash parsing and `/plan` routing.
- Full workspace `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check` results.
- If feasible, a short QA note confirming manual verification of both interactive surfaces.

## Implementation Feedback

Pending implementation review.
