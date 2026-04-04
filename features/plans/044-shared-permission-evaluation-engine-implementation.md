# 044 Shared Permission Evaluation Engine — Implementation Plan

Short summary: Add the deterministic shared permission-evaluation engine in `quine-core` that combines runtime mode, source-partitioned rules, tool-local input analysis, and headless prompt behavior into structured permission outcomes, without yet requiring full approval-routing UX.

## Open Questions

- None. This draft stays scoped to Feature 2 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest QA plan revision; scope, scenarios, and required evidence align with this implementation slice, and no unresolved questions remain.

## Proposed Design

- Anchor the evaluator in the runtime code that already centralizes session/tool execution:
  - `crates/quine-core/src/engine.rs` already owns turn execution, tool dispatch, and `SessionContext`
  - `crates/quine-core/src/channel.rs` already defines the input/output envelope between harness and core
  - `crates/quine-core/src/tool/mod.rs` already provides tool metadata (`is_read_only`, `is_idempotent`) and `ToolError::PermissionDenied`
- Introduce a shared evaluator in `crates/quine-core/src/permission/engine.rs` that accepts:
  - a `PermissionContext` from Feature 1
  - a typed `PermissionRequest`
  - an optional tool-local preliminary decision (`allow`, `deny`, or `defer`)
- Keep precedence deterministic and explicit, matching the design doc and current runtime needs:
  - hard tool-local deny
  - explicit deny rule match
  - explicit allow rule match
  - mode default
  - prompt-behavior fallback for non-interactive contexts
- Represent outcomes as structured internal data rather than raw booleans so later slices can reuse them for diagnostics and approval routing:
  - final decision/outcome
  - matched source or mode
  - human-readable reason string suitable for `ToolError::PermissionDenied`
  - optional rule or request metadata for later diagnostics
- Keep the request model typed and close to actual tool categories already present in `crates/quine-core/src/tool/`:
  - tool name
  - optional action name
  - scope (`Read`, `Write`, `Execute`, `ProcessControl`, `AgentControl`)
  - optional path metadata for filesystem tools
  - optional command metadata for `bash`
  - optional target-session/child-session metadata for `spawn`, `signal`, or `subagent`
- Do not absorb per-tool classification policy into the engine itself; instead, let later Feature 3 tool adapters construct requests while the engine remains the single final arbiter.
- Integrate incrementally with the current runtime loop in `engine.rs`:
  - add internal helpers for evaluating a request before or during tool execution dispatch
  - keep the existing `CoreOutput::ToolRequest`/`ToolResult` flow intact
  - for this slice, integration can remain narrow and synthetic if wiring every tool would force premature scope creep into Feature 3
- Reuse the existing error and interaction vocabulary where possible:
  - denied requests can map cleanly onto `ToolError::PermissionDenied`
  - `ask` decisions should remain structured outcomes for now, even if full pause/resume approval lifecycle lands in Feature 4
  - non-interactive fallback should not silently convert `ask` into `allow`
- Preserve future compatibility with existing inspection and session-state plumbing:
  - `get_session_context` and `CoreCheckpoint` should later be able to expose evaluator results or last-decision data without redesigning the core types
  - outcome/source attribution should be serializable if later slices need it in logs or daemon notifications

## File-by-File Changes

- `crates/quine-core/src/permission/request.rs`
  - Define `PermissionRequest` and typed request metadata structs/enums for tool name, scope, path, command, and process/agent-control context.
- `crates/quine-core/src/permission/outcome.rs`
  - Define structured `PermissionOutcome`, source attribution, matched reason, and any auxiliary explanation fields.
- `crates/quine-core/src/permission/engine.rs`
  - Implement the shared precedence engine and `defer` support.
- `crates/quine-core/src/permission/mod.rs`
  - Wire new request/outcome/engine modules into the permission subsystem.
- `crates/quine-core/src/engine.rs`
  - Add internal evaluator entry points in the existing tool-dispatch path, but keep broad per-tool rollout limited until Feature 3.
  - Optionally capture the most recent permission outcome in `SessionContext` if that simplifies later diagnostics slices without widening public APIs.
- `crates/quine-core/src/tool/mod.rs`
  - Reuse existing `ToolError::PermissionDenied`; add only minimal internal glue if a generic preflight permission hook is needed.
- `crates/quine-core/src/persistence.rs`
  - Only if the implementation stores last-known decision data or prompt behavior snapshots as part of restorable session state.
- `crates/quine-core/src/permission/*.rs` tests
  - Add table-driven unit tests for precedence, `defer`, source attribution, and headless prompt behavior.
- `crates/quine-core/tests/` or adjacent integration tests
  - Add narrow integration coverage for evaluator invocation in the runtime path where already practical.

## Validation Plan

- Table-driven unit tests in `quine-core` for precedence ordering:
  - tool-local deny beats any broader allow
  - explicit deny beats explicit allow
  - explicit allow beats default mode behavior
  - mode default applies when no rules match
- Unit tests for `defer` semantics:
  - tool-local `defer` falls through to rules and mode defaults
  - `defer` with no matching rules produces deterministic mode-driven behavior
- Unit tests for structured outcomes:
  - matched rule source attribution is preserved
  - reason strings are stable and actionable enough to feed `ToolError::PermissionDenied`
  - serializable outcome fields round-trip if they are persisted or surfaced
- Unit/integration tests for non-interactive prompt fallback:
  - a request that would otherwise be `ask` becomes deterministic deny or explicit non-interactive failure per implementation contract
  - no path silently upgrades `ask` to `allow`
- Narrow runtime-path tests in `engine.rs` or integration tests:
  - a synthetic request in a live session context can flow through the evaluator and produce a structured result without changing unrelated tool behavior
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Reviewed `features/plans/044-shared-permission-evaluation-engine-qa.md`; its scope matches this feature slice and stays appropriately limited to the shared evaluator rather than later approval-routing UX.
- The proposed QA scenarios cover the implementation-critical behaviors:
  - precedence correctness
  - `defer` semantics
  - source attribution
  - headless prompt fallback behavior
- The `PermissionOutcome` serialization check is a good fit as long as it remains narrow and tied to fields intentionally exposed by this slice.
- No implementation-side changes are required for plan alignment; the remaining step is for the QA doc to record reciprocal agreement after reviewing this latest implementation-plan revision.
