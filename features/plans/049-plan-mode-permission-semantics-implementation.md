# 049 Plan Mode Permission Semantics — Implementation Plan

Short summary: Formalize Quine plan mode as a permission-state transition with deterministic runtime behavior for read versus mutating tool requests, while preserving previous mode state and avoiding scope creep into future auto-execution modes.

## Open Questions

- None. This draft stays scoped to Feature 7 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Re-reviewed `features/plans/049-plan-mode-permission-semantics-qa.md` after its latest concrete revision. Both docs now align on explicit mode transitions, persisted `plan_mode` compatibility, read-versus-mutating runtime behavior, and the required daemon-backed plan/exit flow, with no unresolved open questions.

## Proposed Design

- Tie plan-mode permission semantics to the current mechanisms already present in the repo:
  - `PersistedSessionConfig.plan_mode` in `crates/quine-core/src/persistence.rs`
  - plan-mode tool filtering in `build_tool_registry_for_session()` and `built_in_tool_definitions()`
  - `EXIT_PLAN_MODE` support in `crates/quine-core/src/channel.rs`, `quine-harness/src/protocol.rs`, and `quine-cli/src/session.rs` / `chat.rs`
  - the planning-only system prompt currently injected from `engine.rs`
  - current `chat` UX that asks `Leave plan mode and start a normal session with this final plan? (y/n)` should remain the outer user contract while core takes over authoritative state transitions
- Promote plan mode from a boolean/tool-filter concept into an explicit permission-mode transition managed by the permission subsystem.
- Keep the compatibility layering explicit:
  - `persisted_config.plan_mode` remains the bootstrap and resume surface used by existing session persistence and CLI code
  - the permission subsystem becomes the source of truth for runtime mode transitions once a session is live
  - `engine.rs` is responsible for keeping those two views coherent during session creation, restore, and `EXIT_PLAN_MODE`
- Centralize plan-mode state transitions in `crates/quine-core/src/permission/mode.rs`:
  - entering plan mode stores the prior permission mode in `pre_plan_mode`
  - leaving plan mode restores the prior mode deterministically
  - repeated/nested transitions behave predictably and do not corrupt prior-mode bookkeeping
- Keep current behavior layered rather than replaced:
  - existing plan-mode tool filtering in `engine.rs` and `tool/mod.rs` remains a coarse safety boundary
  - the new permission semantics add evaluator-level behavior for tools that still exist in plan mode, especially reads versus mutating/process actions
  - current CLI confirmation flow for leaving plan mode in `chat.rs` remains valid, but the underlying core mode state becomes explicit instead of inferred from a boolean alone
- Define plan-mode evaluator defaults conservatively and in terms of existing tool categories:
  - read-only filesystem inspection tools such as `read_file` and `find` continue to behave normally
  - mutating filesystem tools such as `apply_patch` and state-changing execution requests from `bash` become `ask` or `deny` by default depending on the final evaluator contract
  - agent-control tools that could create side effects outside planning should follow the same explicit evaluator path rather than depending only on tool-list filtering
  - `ask_user` and planning-adjacent introspection remain subject to explicit review so plan mode does not dead-end legitimate planning work
  - tool filtering and evaluator defaults should be additive, with filtering preventing obviously out-of-scope tools and evaluator behavior governing the tools that still remain available in plan mode
- Preserve persistence compatibility:
  - existing `plan_mode: bool` in `PersistedSessionConfig` remains the compatibility surface for create/resume flows
  - any richer permission-mode state added in this slice must either derive cleanly from that boolean or be added as optional/defaultable fields
- Ensure the new state interacts cleanly with existing session lifecycle:
  - session creation should initialize permission mode before the first tool request is evaluated
  - `EXIT_PLAN_MODE` should update runtime permission state, persisted compatibility state, and any context inspection surfaces together
  - restored sessions should not drift into contradictory states where `plan_mode` says one thing but the permission context says another
- Avoid auto-mode scope creep:
  - do not add a separate auto-execution mode
  - do not change remote/background approval routing beyond what later features already cover

## File-by-File Changes

- `crates/quine-core/src/permission/mode.rs`
  - Expand explicit plan-mode transition helpers and invariants.
  - Define the single-entry/single-exit bookkeeping around `pre_plan_mode` here so `engine.rs`, persistence restore, and future diagnostics all observe the same behavior.
- `crates/quine-core/src/permission/context.rs`
  - Store current mode and `pre_plan_mode` in a way that remains compatible with current session bootstrap state.
  - Keep the runtime state structured enough that Feature 051 can later expose it through `/context` without inventing a second representation.
- `crates/quine-core/src/permission/engine.rs`
  - Add plan-mode-specific defaults for read vs mutating/process/agent-control requests.
  - Reuse the same permission request shapes produced by `read`, `find`, `write`, and `bash` so plan-mode behavior composes with Features 045, 047, and 048 rather than bypassing them.
- `crates/quine-core/src/engine.rs`
  - Route existing plan-mode entry/exit logic and `EXIT_PLAN_MODE` handling through the permission-mode helpers.
  - Keep current planning system prompt and tool-registry filtering aligned with the explicit permission mode.
  - Ensure resumed sessions that already persist `plan_mode: true` initialize both the compatibility flag and the permission context consistently.
  - Update session snapshots or state-export paths that already expose `plan_mode` so they continue reflecting the authoritative runtime transition.
- `crates/quine-core/src/persistence.rs`
  - Preserve compatibility between persisted `plan_mode` and any richer internal permission-mode state.
  - Keep restore semantics deterministic so checkpoint replay or resume cannot lose the original pre-plan mode.
- `crates/quine-core/src/channel.rs`
  - Reuse the existing `EXIT_PLAN_MODE` input surface and extend it only if transition bookkeeping requires additive metadata.
- `crates/quine-harness/src/protocol.rs`
  - Reuse the existing `EXIT_PLAN_MODE` method; avoid introducing a new RPC unless current semantics cannot express the transition cleanly.
- `crates/quine-cli/src/session.rs`
  - Keep create-session and exit-plan-mode requests aligned with core semantics.
- `crates/quine-cli/src/chat.rs`
  - Preserve current user-visible confirmation flow while relying on the core to own the true mode transition.
  - Keep `/plan` and exit confirmation text stable unless core semantics force a minimal wording change; this feature is about runtime semantics, not a redesigned CLI UX.
- Tests in `quine-core` and CLI/harness integration modules
  - Add transition and runtime-behavior coverage.
  - Favor tests that prove the same session goes from plan-safe behavior back to normal behavior after `EXIT_PLAN_MODE`, rather than isolated one-mode-only fixtures.

## Validation Plan

- Unit tests for permission-mode transitions:
  - entering plan mode stores the previous mode once
  - leaving plan mode restores the prior mode and clears `pre_plan_mode` as intended
  - repeated or nested transitions remain deterministic and non-corrupting
- Integration tests in `quine-core`:
  - plan-mode evaluator defaults treat read tools differently from mutating/process tools
  - tool filtering and evaluator behavior do not contradict each other for plan-mode sessions
- Session/bootstrap tests:
  - sessions created with `plan_mode: true` initialize permission context in `Plan`
  - `EXIT_PLAN_MODE` transitions both the persisted/runtime flag and the permission mode coherently
- CLI/harness path tests where existing coverage patterns allow:
  - exiting plan mode through the current RPC and chat flow restores normal session behavior
  - daemon-backed coverage should explicitly prove the existing `/plan` flow still reaches the same confirmation prompt while the underlying permission state becomes more precise
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Re-reviewed `features/plans/049-plan-mode-permission-semantics-qa.md` after its latest revision.
- The QA plan now matches this implementation plan’s concrete interaction points with existing code:
  - exact daemon-backed `/plan` and `EXIT_PLAN_MODE` flows
  - focused tests for `pre_plan_mode` preservation and restoration
  - explicit read-versus-mutating tool expectations in a live plan-mode session
- Scope remains aligned: explicit permission-mode transitions in `quine-core`, compatibility with persisted `plan_mode`, reuse of current CLI confirmation UX, and no expansion into future auto-execution behavior.
- No further QA-side changes are required from this review.
