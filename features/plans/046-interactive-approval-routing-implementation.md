# 046 Interactive Approval Routing — Implementation Plan

Short summary: Turn `ask` permission outcomes into a concrete pause/resume approval workflow across `quine-core`, `quine-harness`, and `quine-cli`, including pending-request tracking, approval responses, and safe failure behavior for unresolved prompts.

## Open Questions

- None. This draft stays scoped to Feature 4 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

pending — Re-reviewed the latest `features/plans/046-interactive-approval-routing-qa.md` revision. Scope alignment is good, but agreement remains blocked until the QA doc adds the concrete daemon commands, exact chat messages, operator responses, and expected outputs required by `.claude/commands/feature-planning.md`.

## Proposed Design

- Build the approval lifecycle on top of the interaction and session-state primitives that already exist today:
  - `ExecutionContext` in `crates/quine-core/src/tool/mod.rs` already carries an optional `InteractionChannel`
  - `CoreOutput::InteractionNeeded` and `CoreInput::InteractionResponse` already exist in `crates/quine-core/src/channel.rs`
  - `SessionContext` in `crates/quine-core/src/engine.rs` already tracks `pending_interaction`
  - `submit_interaction_response` already exists in `crates/quine-harness/src/service.rs` and `protocol.rs`
  - `quine-cli/src/chat.rs` already handles interactive tool prompts coming from `INTERACTION_NEEDED`
- Reuse this existing ask/response transport rather than inventing a completely separate approval RPC for the first slice.
- Introduce a permission-specific pending-approval model that sits alongside the current generic interaction plumbing:
  - when the shared evaluator returns `ask`, core creates a permission approval request with stable correlation metadata
  - core emits an interaction/approval prompt to the harness/CLI via the existing channel surface, using permission-specific wording and options
  - core pauses the affected tool execution until a matching response arrives or cancellation/timeout occurs
- Keep ownership aligned to current crate responsibilities:
  - `quine-core` owns correlation between an `ask` decision, paused tool execution, and resumed/denied completion
  - `quine-harness` forwards the existing interaction notification and response methods without becoming a policy engine
  - `quine-cli` renders local operator choices using its existing interactive event loop and response submission path
- Extend the current interaction model only as far as needed for permission approvals:
  - reuse `InteractionRequest.kind`, options, and `allow_freeform` where possible
  - include enough hidden/structured correlation in core state so the response maps back to the paused permission request rather than only to a generic tool prompt
  - if needed, add a dedicated source label or typed discriminator so CLI rendering can distinguish permission approval prompts from general `ask_user` prompts
- Support concrete first-release responses from the design doc:
  - approve once
  - deny once
  - optionally approve and add a session-scoped rule if Feature 1 rule storage already supports it cleanly
- Integrate with current cancellation/session semantics in `engine.rs`:
  - paused approval state should coexist with existing `cancel_tx`, `interrupted`, and `SessionState` transitions
  - denial should map cleanly onto `ToolError::Cancelled` or `ToolError::PermissionDenied` according to the final runtime contract
  - timeout/unreachable-responder behavior must resolve the paused state deterministically
- Preserve current checkpoint boundaries in `persistence.rs`:
  - do not make mid-approval transient state persistable unless the existing session restore model can support it coherently
  - if pauses cannot be restored safely in this slice, the plan should explicitly constrain persisted states rather than serializing half-finished approval workflows

## File-by-File Changes

- `crates/quine-core/src/permission/approval.rs`
  - Add internal approval request IDs, pending approval state, and response-resolution helpers for `ask` outcomes.
- `crates/quine-core/src/engine.rs`
  - Extend `SessionContext` with permission-pending state alongside the existing `pending_interaction` sender.
  - Pause/resume tool execution when permission approval is required.
  - Route interaction responses to the correct pending approval and complete the original tool path.
- `crates/quine-core/src/channel.rs`
  - Reuse `InteractionNeeded` / `InteractionResponse`; add only additive fields if permission-specific correlation cannot be represented with current request/response envelopes.
- `crates/quine-core/src/tool/mod.rs`
  - Ensure permission-driven pauses cooperate with `CancellationChannel` and existing interactive tool semantics.
- `crates/quine-harness/src/service.rs`
  - Keep `submit_interaction_response` as the narrow harness interface unless a small permission-specific helper is justified by implementation clarity.
- `crates/quine-harness/src/protocol.rs`
  - Reuse `SUBMIT_INTERACTION_RESPONSE` and `INTERACTION_NEEDED` if possible; add fields only if the CLI needs stronger permission-prompt discrimination.
- `crates/quine-harness/src/server.rs`
  - Preserve notification forwarding and JSON-RPC response handling for approval prompts.
- `crates/quine-cli/src/chat.rs`
  - Add permission-aware rendering for incoming interaction requests so approve/deny choices are clear and consistent.
  - Route operator responses back through the existing interaction submission path.
- `crates/quine-cli/src/render.rs` and/or TUI modules
  - If needed, add dedicated prompt presentation helpers so permission approvals are visually distinct from generic tool questions.
- Tests across `quine-core`, `quine-harness`, and `quine-cli`
  - Add pause/resume, deny, timeout, and prompt-rendering coverage.

## Validation Plan

- `quine-core` integration tests for permission approval lifecycle:
  - an `ask` outcome pauses the tool execution and records pending approval state
  - approve-once resumes the original tool path successfully
  - deny-once resolves the tool path deterministically and clears pending state
  - timeout or cancellation resolves the pending approval without leaving the session wedged
- `quine-harness` tests for transport:
  - `INTERACTION_NEEDED` notifications carry permission approval prompts to subscribers
  - `SUBMIT_INTERACTION_RESPONSE` responses reach the paused session and resume/deny the correct request
- `quine-cli` tests for rendering and operator flow:
  - permission prompts are distinguishable from general `ask_user` prompts
  - approve/deny options map to the expected submitted response payloads
- Daemon-backed scenarios:
  - at least one real interactive session that triggers a permission prompt, receives an approval response, and completes the turn
  - at least one deny or no-responder scenario proving deterministic non-success behavior
- Persistence/coherence checks:
  - verify checkpointing either excludes mid-approval sessions or restores them coherently if this slice chooses to support that
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Reviewed `features/plans/046-interactive-approval-routing-qa.md`; the scenario set is directionally aligned with this implementation plan's scope and lifecycle model.
- Agreement is still blocked by missing execution detail in the QA doc. Per `.claude/commands/feature-planning.md`, the daemon-backed scenarios need concrete, executable steps rather than abstract bullets.
- The QA doc should revise the daemon scenarios to include:
  - the exact daemon and client commands to start the local service and connect to the chat session
  - the exact user message(s) that trigger the permission `ask` path
  - the exact operator response payload or interactive selection used to approve or deny
  - the expected observable outputs for each round, including the approval prompt text or status, tool activity, final assistant response, and the deterministic timeout/no-responder result
- The optional “approve and add session rule” scenario should stay explicitly conditional on that behavior being included in this slice, matching the implementation plan's current scope note.
- Once those specifics are added and no new open questions appear, this plan can move to `agreed` after re-reviewing the QA doc's latest revision.
