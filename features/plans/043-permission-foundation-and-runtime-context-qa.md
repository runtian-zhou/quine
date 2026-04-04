# 043 Permission Foundation and Runtime Context — QA Plan

Short summary: Verify Quine Feature 1 permission foundation work, including internal permission-domain types, default `PermissionContext` initialization, additive runtime mutation helpers, conservative persistence behavior, and narrow harness bootstrap wiring.

## Open Questions

- None. This QA plan stays scoped to Feature 1 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

pending — Reviewed the latest implementation-plan revision. Scope is aligned to Feature 1 only, but agreement remains blocked until the implementation doc confirms this tightened QA plan and both docs record the same executable validation details.

## Test Strategy

- Validate at three layers:
  - `quine-core` unit tests for domain types, defaults, mutations, mode transitions, and any serde added by this slice
  - narrow `quine-harness` bootstrap tests around `LocalHarness` and checkpoint/session-context projection
  - one concrete multi-round local-daemon chat scenario, because this slice changes `quine-core`
  - workspace quality gates from `CLAUDE.md`
- Keep pass criteria limited to:
  - permission enums and structs
  - `PermissionContext` as runtime session state
  - source-partitioned rules
  - additional allowed directories
  - prompt/headless behavior flags
  - plan-mode prior-state bookkeeping
  - reconstructibility or observability of initialized permission state through current checkpoint/session-context surfaces
- Exclude later-slice behavior:
  - no evaluator precedence testing
  - no per-tool permission integration
  - no interactive approval routing
  - no user-visible prompt workflow requirements beyond existing `/context` inspection output
- Prefer deterministic assertions over broad chat-flow testing.
- Require focused test names so QA can run exact selectors rather than broad crate-wide filters. If the implementation introduces different names, it should preserve equivalent coverage and update both docs before agreement.

## Scenarios

- **Unit — Default Permission Context**
  - Add a focused `quine-core` unit test named `permission::context::tests::default_permission_context_is_conservative` or an equivalently precise selector.
  - Run `cargo test -p quine-core default_permission_context_is_conservative -- --exact --nocapture`.
  - Expect assertions for: default non-plan mode when bootstrapped without `plan_mode`, empty rules partitioned by source, empty additional allowed roots, explicit prompt behavior, and `pre_plan_mode == None` unless the design intentionally stores a deterministic equivalent default.
- **Unit — Rule Insertion by Source**
  - Add a focused `quine-core` unit test named `permission::context::tests::permission_rules_remain_partitioned_by_source`.
  - Run `cargo test -p quine-core permission_rules_remain_partitioned_by_source -- --exact --nocapture`.
  - Expect each inserted rule to remain in its source bucket with its original provenance intact and with no mutation to unrelated source partitions.
- **Unit — Additional Directory Handling**
  - Add a focused `quine-core` unit test named `permission::context::tests::additional_allowed_roots_append_without_side_effects`.
  - Run `cargo test -p quine-core additional_allowed_roots_append_without_side_effects -- --exact --nocapture`.
  - Expect additive directory storage, stable ordering or explicitly documented deterministic normalization, and no mutation to mode, prompt behavior, or rule partitions.
- **Unit — Plan-Mode Bookkeeping**
  - Add a focused `quine-core` unit test named `permission::mode::tests::plan_mode_transitions_preserve_prior_mode`.
  - Run `cargo test -p quine-core plan_mode_transitions_preserve_prior_mode -- --exact --nocapture`.
  - Expect entering plan mode from a non-plan mode to store the prior mode, exiting plan mode to restore it, and repeated enter/exit calls to return a deterministic `ModeTransitionResult` without corrupting stored state.
- **Unit — Serde / Persistence**
  - Only if this slice persists new permission fields, add a focused test named `persistence::tests::permission_foundation_fields_round_trip_checkpoint`.
  - Run `cargo test -p quine-core permission_foundation_fields_round_trip_checkpoint -- --exact --nocapture`.
  - Expect backward-compatible deserialize behavior for older checkpoints plus round-trip preservation of any newly persisted permission fields.
  - If no new persisted permission fields are added, QA records this scenario as not applicable and instead cites the bootstrap reconstruction coverage from the next two scenarios.
- **Integration — Harness Bootstrap Test**
  - Add a colocated `quine-harness` test in `crates/quine-harness/src/local.rs` or `crates/quine-harness/src/storage.rs` named `tests::create_session_bootstraps_permission_context_without_explicit_inputs`.
  - Run `cargo test -p quine-harness create_session_bootstraps_permission_context_without_explicit_inputs -- --exact --nocapture`.
  - Start from the existing `LocalHarness::create_session` path with no new permission-specific config.
  - Expect session creation success plus one of the following observable proofs, using current codebase surfaces only:
    - if the implementation exposes permission foundation fields through checkpoint-derived session context, assert those fields directly
    - otherwise, assert the checkpoint/session snapshot still contains the expected existing bootstrap facts (`plan_mode`, `working_directory`, session creation succeeds) and pair that with direct core-unit assertions proving `PermissionContext` is reconstructed from those inputs
- **Multi-round Local Daemon — `/context` Bootstrap Evidence**
  - Use a real local daemon and the existing interactive chat client.
  - Start the daemon in one shell: `cargo run --bin quine -- daemon start --socket /tmp/quine-043.sock`.
  - In a second shell, run a two-round session with exact stdin: `printf '/context\n/quit\n' | cargo run --bin quine -- chat --socket /tmp/quine-043.sock --plan`.
  - Round 1 message: `/context`.
  - Expected round 1 output:
    - a line starting with `Session created: ` followed by a session id
    - pretty-printed JSON session context from the existing checkpoint-derived `/context` command
    - JSON must include `"session_id"`, `"working_directory"`, and `"plan_mode": true`
    - if the implementation exposes permission-foundation fields through this snapshot, they must show initialized defaults consistent with the unit tests
    - no tool calls, no approval prompts, and no error text
  - Round 2 message: `/quit`.
  - Expected round 2 output:
    - chat exits cleanly without an error
    - no extra tool activity is emitted
  - After chat exits, stop the daemon with `cargo run --bin quine -- daemon stop --socket /tmp/quine-043.sock`.
  - If permission context is not directly visible in `/context`, QA must explicitly record that this scenario verifies the live bootstrap path and current observable session facts only, while the focused `quine-core` and `quine-harness` tests provide the direct permission-state assertions.

## Required Evidence

- Passing focused test results, captured as exact command lines and pass/fail output, for:
  - `cargo test -p quine-core default_permission_context_is_conservative -- --exact --nocapture`
  - `cargo test -p quine-core permission_rules_remain_partitioned_by_source -- --exact --nocapture`
  - `cargo test -p quine-core additional_allowed_roots_append_without_side_effects -- --exact --nocapture`
  - `cargo test -p quine-core plan_mode_transitions_preserve_prior_mode -- --exact --nocapture`
  - `cargo test -p quine-core permission_foundation_fields_round_trip_checkpoint -- --exact --nocapture` if and only if new persisted permission fields are introduced
  - `cargo test -p quine-harness create_session_bootstraps_permission_context_without_explicit_inputs -- --exact --nocapture`
- A transcript or captured stdout/stderr for the local-daemon scenario showing:
  - `cargo run --bin quine -- daemon start --socket /tmp/quine-043.sock`
  - `printf '/context\n/quit\n' | cargo run --bin quine -- chat --socket /tmp/quine-043.sock --plan`
  - `cargo run --bin quine -- daemon stop --socket /tmp/quine-043.sock`
- One concrete live-session assertion bundle showing either:
  - permission-foundation fields directly in `/context` output, or
  - current observable bootstrap facts from `/context` plus the exact focused unit/bootstrap tests proving reconstructibility of `PermissionContext`
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Reviewed the latest implementation plan revision. Its scope remains aligned to Feature 1 only.
- This QA doc now assumes the implementation will provide focused tests with exact selectors equivalent to the names listed above; if the implementation chooses different test names or placements, both docs must be updated before agreement.
- The implementation plan is compatible with current codebase reality:
  - `get_session_context` remains checkpoint-derived session inspection, not a new permission RPC
  - `quine-harness` bootstrap coverage is expected as colocated `#[cfg(test)]` coverage in `crates/quine-harness/src/local.rs` or `storage.rs`
- No additional scope changes are requested. Remaining coordination step is for the implementation doc to confirm this tightened executable QA plan and then update both docs to matching agreement status.
