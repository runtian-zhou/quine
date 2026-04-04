---
status: pending
---

# Interactive Approval Routing

Turn permission `ask` outcomes into a real interactive pause/resume workflow so
operators can approve or deny a gated tool call through the existing
interaction channel.

## Requirements

- Reuse `InteractionNeeded` and `InteractionResponse` for permission approvals.
- Pause tool execution when the evaluator returns `RequiresApproval`.
- Resume only on explicit one-time approval and deny deterministically on
  explicit rejection or invalid approval input.
- Keep pending approval state inside the session runtime so the request can be
  cancelled cleanly.
- Surface approval prompts distinctly enough that the CLI can render them as
  permission prompts instead of generic `ask_user` requests.
- Add focused core and harness coverage for approve and deny flows.

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- A permission-gated tool call emits `InteractionNeeded`.
- `approve once` resumes the paused tool exactly once.
- `deny once` resolves the turn without running the gated action.
- Cancellation clears pending approval state instead of wedging the session.
