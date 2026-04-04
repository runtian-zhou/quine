# 051 Permission Diagnostics and Inspection — Implementation Plan

Short summary: Make Quine’s permission system observable by adding structured decision explanations and operator-visible inspection surfaces for runtime permission state, loaded rules, pending approvals, and recent denial/prompt reasons.

## Open Questions

- None. This draft stays scoped to Feature 9 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Re-reviewed `features/plans/051-permission-diagnostics-and-inspection-qa.md` after updating both docs to concrete inspection scenarios. Both docs now align on extending existing `GET_SESSION_CONTEXT` and `/context` surfaces, concrete daemon-backed inspection flows, and there are no unresolved open questions.

## Proposed Design

- Build diagnostics on top of the context and notification surfaces that already exist:
  - `GET_SESSION_CONTEXT` is already a first-class RPC method in `crates/quine-harness/src/protocol.rs`
  - `quine-cli/src/context_debug.rs` already defines and renders a `SessionContextSnapshot`
  - `CoreOutput` in `crates/quine-core/src/channel.rs` already carries tool results, interaction-needed events, session errors, and plan progress
  - this feature should extend those snapshots rather than introducing a parallel permission-debug transport or CLI command
- Prefer extending existing context/inspection snapshots rather than adding a parallel permission-debug API.
- Add structured permission explanation data in `quine-core` that can be attached to runtime state and surfaced through existing inspection flows:
  - current permission mode
  - rule partitions by source
  - additional allowed roots
  - prompt behavior
  - any pending permission approval summary
  - last permission decision summary for denied or prompted requests
- Model the snapshot so later Features 052 and beyond can add persisted-rule provenance without replacing the inspection shape a second time.
- Keep the first release textual and operational:
  - no dedicated management TUI
  - no editable policy dashboard
  - just enough structured state so operators and QA can understand why an action was allowed, denied, or prompted
- Integrate explanation data close to the evaluator and session runtime:
  - `PermissionOutcome` should carry machine-serializable source/reason fields
  - `engine.rs` can stash the last meaningful permission outcome in `SessionContext` if that is the cleanest path to inspection
  - pending approval state from Feature 4 should become inspectable through the same session-context snapshot rather than a special-case UI path
  - keep the inspected state compact enough that `GET_SESSION_CONTEXT` remains a practical operational debugging call rather than a full replay export
- Extend the existing CLI context renderer rather than inventing a new command:
  - add permission-related fields to `SessionContextSnapshot` in `crates/quine-cli/src/context_debug.rs`
  - rely on the existing `/context` command path in `chat.rs` to render them
- Keep sensitive output bounded:
  - expose rule sources, targets, and reasons at an operationally useful level
  - avoid dumping unnecessary hidden/internal state that is not actionable for debugging or QA

## File-by-File Changes

- `crates/quine-core/src/permission/outcome.rs`
  - Add stable structured explanation fields for permission decisions.
- `crates/quine-core/src/permission/context.rs`
  - Expose inspectable permission-state summaries such as current mode, rules by source, and additional roots.
  - Reuse the same runtime state that the evaluator mutates so inspection reflects live truth rather than a separately synthesized debug view.
- `crates/quine-core/src/engine.rs`
  - Store and expose the latest permission-relevant diagnostic state in `SessionContext`.
  - Update the snapshot after both denied and prompted outcomes so operators can inspect the last meaningful permission decision even if the tool never executed.
- `crates/quine-core/src/channel.rs`
  - Reuse existing output events; add new notification variants only if existing context inspection cannot convey necessary permission state.
- `crates/quine-harness/src/service.rs`
  - Extend `get_session_context()` implementation to include permission snapshot fields.
  - Keep the RPC additive and backward compatible by extending the existing JSON shape returned to the CLI rather than adding a second permission-only endpoint.
- `crates/quine-harness/src/protocol.rs`
  - Reuse `GET_SESSION_CONTEXT`; avoid adding a new method unless strictly required.
- `crates/quine-cli/src/context_debug.rs`
  - Extend `SessionContextSnapshot` with permission-related fields and update rendering accordingly.
  - Keep the output readable in both raw JSON and the current `/context` rendering path so QA can verify fields without special tooling.
- `crates/quine-cli/src/chat.rs`
  - Continue to expose permission diagnostics through the existing `/context` command flow.
- Tests across core, harness, and CLI snapshot layers
  - Add outcome-serialization and context-inspection parity coverage.

## Validation Plan

- Unit tests for permission explanation serialization:
  - allow, deny, and ask outcomes preserve source and reason fields
  - structured explanations remain stable enough for snapshot-based inspection
- Integration tests for session-context inspection:
  - a live session with known mode, rules, and additional roots produces matching `GET_SESSION_CONTEXT` output
  - a denied or prompted request updates the latest permission-decision summary in the inspected session snapshot
  - pending approval state, when present, appears in the same snapshot path
- CLI snapshot tests:
  - `SessionContextSnapshot` in `context_debug.rs` serializes/deserializes with the new permission fields
  - `/context` rendering remains readable after the new fields are added
- QA-executable coverage expected from the paired QA plan:
  - at least one concrete local-daemon or one-off chat scenario with exact startup commands, exact prompts/messages, and expected inspection output for `/context`
  - one scenario that exercises a denied request and verifies the surfaced source/reason fields
  - one scenario that exercises a prompted request or pending approval and verifies it appears in the inspected session state
  - a concrete `GET_SESSION_CONTEXT`/CLI parity check proving the harness snapshot and CLI rendering show the same permission facts
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Re-reviewed `features/plans/051-permission-diagnostics-and-inspection-qa.md` after its latest revision.
- The QA plan now satisfies the workflow’s concreteness requirements:
  - it defines exact `cargo test` targets for outcome serialization and context inspection coverage
  - it includes exact daemon startup and CLI commands for `/context`/inspection flows
  - it spells out concrete denied-action and pending-approval scenarios, including the expected surfaced fields for mode, rule sources, pending approvals, and last decision reasons
- Scope remains aligned with this implementation plan: extend existing `GET_SESSION_CONTEXT` and `/context` surfaces rather than inventing a new diagnostics command or UI.
- No further QA-side changes are required from this review.
