---
status: pending
---

# Plan Mode Permission Semantics

Formalize Quine plan mode as an explicit permission-state transition so live
sessions preserve their pre-plan mode, keep read-only planning work available,
and block mutating behavior deterministically until plan mode is exited.

## Requirements

- Preserve the prior permission mode when a session enters plan mode.
- Restore that prior mode deterministically when the existing `EXIT_PLAN_MODE`
  path is used.
- Keep persisted `plan_mode` bootstrap and runtime permission state coherent for
  create, restore, and exit flows.
- Allow representative read-only tool activity in plan mode without spurious
  permission prompts.
- Deny or otherwise conservatively block representative mutating requests in
  plan mode without running them.
- Add focused core, harness, and CLI coverage for plan-mode bootstrap,
  transition, and runtime behavior.

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Sessions bootstrapped in plan mode retain a deterministic `pre_plan_mode`.
- Exiting plan mode restores normal runtime behavior in the same session.
- Read-only plan-mode requests still execute successfully.
- Mutating plan-mode requests do not execute as normal allowed actions.
