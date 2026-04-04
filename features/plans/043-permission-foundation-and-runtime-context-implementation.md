# 043 Permission Foundation and Runtime Context — Implementation Plan

Short summary: Establish Quine’s internal permission vocabulary and the session-scoped `PermissionContext` in `quine-core`, plus the minimal harness bootstrap wiring needed to initialize that state, without yet adding policy evaluation, tool enforcement, or approval routing.

## Open Questions

- None. This draft is intentionally limited to Feature 1 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

pending — Reviewed the latest QA draft. The paired QA plan is directionally aligned on scope, but it still needs concrete executable scenario details and evidence targets before both docs can be marked `agreed`.

## Proposed Design

- Scope this feature to the current runtime seams that already exist in the repo:
  - `quine-core/src/engine.rs` already owns the per-session `SessionContext`
  - `quine-core/src/channel.rs` already carries `CreateSession` bootstrap inputs
  - `quine-harness/src/config.rs` already defines `SessionConfig`
  - `quine-cli/src/session.rs` already builds `create_session` params with `plan_mode`
- Add a dedicated internal permission subsystem under `crates/quine-core/src/permission/` so the later permission slices plug into one crate-owned runtime model instead of scattering state across `engine.rs`, tools, and harness transport code.
- Keep the first slice internal-first and additive:
  - do not change the `Tool` trait in `crates/quine-core/src/tool/mod.rs`
  - do not change `HarnessService` trait shape in `crates/quine-harness/src/service.rs`
  - do not add new RPC methods in `crates/quine-harness/src/protocol.rs`
  - do not yet gate tool execution on permission decisions
- Introduce the foundational permission vocabulary described in the design doc, with crate-private visibility by default:
  - `PermissionMode` with at least `Default`, `AcceptEdits`, `Plan`, and `Bypass`
  - `PermissionDecision`, `PermissionRuleEffect`, `PermissionRuleSource`, and `PermissionScope`
  - `PermissionTarget`, `PermissionRule`, and `PermissionRuleSet`
  - `ApprovalRequestId`, `PermissionPromptBehavior`, and `ModeTransitionResult`
  - `PermissionContext` as the authoritative per-session runtime object
- Make `PermissionContext` explicitly model only the state this slice can initialize today from current code paths:
  - current permission mode, derived from existing `plan_mode` bootstrap state plus a non-plan default
  - optional `pre_plan_mode` to preserve current `exit_plan_mode` semantics in a future-safe way
  - rule partitions keyed by source, initially empty unless later bootstrap paths seed them
  - workspace root and additional allowed roots, seeded from the existing working-directory/session-filesystem setup
  - prompt behavior derived from session interactivity assumptions in harness/CLI startup, even if initially a simple default enum
  - optional lightweight sandbox/bootstrap snapshot fields, but only if they can be populated from current harness startup data without inventing unsupported policy config
- Keep mutation logic concentrated in helper methods on `PermissionContext` rather than open-coded field edits inside `engine.rs`:
  - add rule into a source partition
  - append an additional allowed directory
  - enter and exit plan mode while preserving previous mode bookkeeping
- Tighten this slice to state the current codebase can actually expose and verify now:
  - `plan_mode` already exists in `PersistedSessionConfig`, `CoreInput::CreateSession`, `SessionConfig`, and CLI session creation; use that as the only required mode bootstrap input for this feature
  - do not add user-facing permission flags, RPC methods, or harness trait changes in this slice
  - do not add approval request lifecycle state yet; `ApprovalRequestId` may exist as a type, but `PermissionContext` should not gain active approval workflow semantics until a later feature
  - only add persistence fields if the session checkpoint must restore permission state beyond what `PersistedSessionConfig.plan_mode` and `working_directory` already restore today
- Integrate with the existing runtime session model by storing `PermissionContext` directly in `engine.rs`’s `SessionContext` struct beside existing fields such as `working_directory`, `filesystem`, `pending_interaction`, and `persisted_config`.
- Extend persistence conservatively:
  - `crates/quine-core/src/persistence.rs` currently persists `PersistedSessionConfig` and `PersistedSessionState`
  - only add optional/defaultable permission foundation fields if they are truly needed to restore plan-mode-adjacent permission state or bootstrap snapshots
  - avoid persisting transient request state, evaluation caches, or future approval-lifecycle data in this slice
- Bootstrap from current harness inputs instead of adding a new user-facing permission configuration surface:
  - `SessionConfig` in `crates/quine-harness/src/config.rs` can carry additive fields later, but this slice should work even with no new config
  - `CoreInput::CreateSession` in `crates/quine-core/src/channel.rs` is the natural handoff point for initial permission context construction
  - sessions created through `quine-cli/src/session.rs` must continue to work unchanged when no permission-specific params are present

## File-by-File Changes

- `crates/quine-core/src/lib.rs`
  - Add the new internal permission module export in the narrowest way needed for core-internal use and any harness bootstrap glue.
- `crates/quine-core/src/permission/mod.rs`
  - Create the new permission subsystem root and organize `types`, `context`, and `mode` submodules.
- `crates/quine-core/src/permission/types.rs`
  - Define the core enums and structs for permission modes, rule sources, scopes, targets, rule storage, prompt behavior, and approval identifiers.
- `crates/quine-core/src/permission/context.rs`
  - Define `PermissionContext`, defaults, rule/source insertion helpers, additional-root helpers, and any minimal sandbox snapshot container.
- `crates/quine-core/src/permission/mode.rs`
  - Implement explicit plan-mode entry/exit bookkeeping helpers and `ModeTransitionResult`.
- `crates/quine-core/src/engine.rs`
  - Add `PermissionContext` to the existing `SessionContext` struct.
  - Initialize it during session creation from `CoreInput::CreateSession` data and current working-directory/session bootstrap state.
  - Route existing `exit_plan_mode` handling through explicit permission-mode helpers so future plan-mode policy is centralized.
- `crates/quine-core/src/channel.rs`
  - If needed, extend `CreateSession` with additive permission-bootstrap fields only when they can be sourced from existing harness config without destabilizing callers.
- `crates/quine-core/src/persistence.rs`
  - Prefer leaving `PersistedSessionConfig` unchanged in this slice.
  - Only add optional/defaultable permission fields if implementation proves `PermissionContext` cannot be reconstructed on restore from persisted `working_directory`, persisted `plan_mode`, and default rule/prompt state.
- `crates/quine-harness/src/config.rs`
  - Keep `SessionConfig` unchanged unless implementation proves a currently-existing bootstrap input must be modeled explicitly for restore parity.
- `crates/quine-harness/src/local.rs`
  - Reuse the existing `SessionConfig -> CoreInput::CreateSession` forwarding path.
  - Avoid additive bootstrap fields unless they are sourced from real existing config/state rather than invented permission configuration.
- `crates/quine-cli/src/session.rs`
  - Expect no changes for this slice unless the implementation discovers a narrow compatibility bug in the existing `plan_mode` request-building path.
- `crates/quine-core/src/permission/*.rs` tests
  - Add colocated unit tests for defaults, rule insertion, additional roots, mode transitions, and serde behavior.
- `crates/quine-harness/tests/` or existing integration test modules
  - Add a narrow session-bootstrap test only if adjacent harness patterns already validate live session initialization.

## Validation Plan

- Unit tests in `quine-core` for deterministic defaults:
  - default `PermissionContext` mode and prompt behavior
  - empty `PermissionRuleSet` partitions
  - empty additional-root list and unset `pre_plan_mode`
- Unit tests in `quine-core` for additive mutation helpers:
  - inserting rules preserves source partitioning
  - adding additional roots is additive and deterministic
  - entering/exiting plan mode stores and restores prior state predictably
- Serde tests for any newly persisted types in `persistence.rs`:
  - round-trip any persisted permission foundation fields
  - verify backward-compatible defaults if old checkpoints omit those fields
- Narrow runtime/bootstrap verification:
  - create a session through the existing harness path and assert a valid permission context exists in core session state or in an exported checkpoint/session-context snapshot if permission data is made observable there
  - verify sessions created without any new permission-specific inputs still initialize successfully
  - if no new permission data is exposed through `get_session_context`, cover bootstrap initialization through a `quine-harness` unit test around `LocalHarness`/checkpoint construction rather than inventing a new inspection RPC
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Reviewed the latest `features/plans/043-permission-foundation-and-runtime-context-qa.md` revision after tightening.
- The QA plan is now concrete and executable within current codebase constraints:
  - exact focused test-command shapes are specified for `quine-core` and colocated `quine-harness` bootstrap coverage
  - the required `quine-core` live-session scenario is defined as a real local-daemon `/context` check using `cargo run --bin quine -- daemon start`, piped `quine chat`, and daemon shutdown
  - expected observable evidence is grounded in today’s checkpoint-derived session-context output, with explicit fallback language when permission fields remain reconstructible rather than directly exposed
- No further QA-plan changes are required from this side unless implementation chooses different exact test names or relocates the bootstrap coverage, in which case both docs should be updated together before agreement.
- Agreement should remain `pending` until this implementation doc is refreshed to reflect the now-concrete QA plan and both docs are marked consistently after latest-revision review.
