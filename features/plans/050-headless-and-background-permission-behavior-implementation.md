# 050 Headless and Background Permission Behavior — Implementation Plan

Short summary: Define deterministic non-interactive permission behavior for headless, scheduled, and background Quine sessions so unresolved prompts fail safely and surface clear operator-visible outcomes.

## Open Questions

- None. This draft stays scoped to Feature 8 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Re-reviewed `features/plans/050-headless-and-background-permission-behavior-qa.md` after its latest concrete revision. Both docs now align on explicit prompt-behavior modeling, deterministic fail-safe handling for non-interactive sessions, exact daemon/CLI validation, and no unresolved open questions remain.

## Proposed Design

- Build non-interactive behavior on top of session/bootstrap surfaces that already exist today:
  - `SessionConfig` in `crates/quine-harness/src/config.rs` already distinguishes session startup inputs
  - `CoreInput::CreateSession` in `crates/quine-core/src/channel.rs` is the natural place to mark session prompt behavior
  - `quine-cli/src/chat.rs` is interactive, while one-shot execution paths can represent headless flows
  - scheduled/background execution already has seeds in `scheduler.rs`, `planner/scheduler.rs`, and harness scheduling methods
  - session inspection surfaces such as `get_session_context` and CLI status commands should be able to expose the resulting non-interactive state without a separate debugging path
- Make prompt behavior an explicit part of `PermissionContext` rather than inferring it ad hoc from whichever frontend happened to create the session.
- Keep the behavior derivation close to existing startup paths:
  - `quine chat` should mark sessions interactive at creation time
  - one-shot `send_message`/run-style flows should mark sessions headless unless there is a real responder path attached
  - scheduler-created child sessions should mark themselves background/non-interactive before the first permission evaluation
- Define a small first-release prompt-behavior model in `quine-core`:
  - interactive local session
  - non-interactive/headless session
  - scheduled/background session
- Apply fail-safe behavior in the shared evaluator and approval lifecycle:
  - if a request resolves to `ask` and the session prompt behavior is non-interactive, the request becomes deterministic deny or explicit failure/cancellation according to the agreed runtime contract
  - no headless path silently upgrades `ask` to `allow`
  - background/scheduled flows follow the same deterministic policy instead of hanging on an unreachable responder
  - the outcome should remain distinguishable from transport failure so session inspection, CLI status, and future diagnostics can report a permission-specific cause
- Keep this feature integrated with the current event model rather than creating a parallel non-interactive path:
  - interactive sessions still emit the current interaction-needed behavior
  - headless/background sessions should short-circuit before stale pending approval state is recorded
  - the same `ToolError::PermissionDenied` and session-error surfaces should carry the final operator-visible outcome
- Thread the behavior from existing creation paths:
  - interactive chat and TUI sessions set interactive prompt behavior
  - one-shot run flows set non-interactive behavior unless there is an established interactive responder path
  - scheduled agent runs set scheduled/background behavior when they create child sessions
- Surface clear operator-visible outcomes without inventing a separate permission UI:
  - `ToolError::PermissionDenied` and session errors should remain distinguishable from infrastructure failures
  - CLI text and daemon notifications should make it obvious that a prompt-required action was blocked because the session could not ask
  - `get_session_context` and status-style inspection should be able to show that the session finished or failed deterministically rather than remaining approval-pending forever
- Keep future remote-responder support out of scope:
  - model prompt behavior so a later remote responder can be added cleanly
  - do not add new networked approval actors in this slice

## File-by-File Changes

- `crates/quine-core/src/permission/context.rs`
  - Expand prompt-behavior state to represent interactive, headless, and scheduled/background modes explicitly.
  - Keep this representation serializable or snapshot-friendly enough that session inspection can later expose it directly.
- `crates/quine-core/src/permission/engine.rs`
  - Apply deterministic non-interactive fallback for `ask` outcomes.
  - Reuse the same approval-evaluation path used by interactive sessions so only the fallback behavior changes, not rule precedence or matching.
- `crates/quine-core/src/engine.rs`
  - Initialize session prompt behavior from create-session inputs and propagate it through approval/evaluation flow.
  - Ensure would-be approval requests in non-interactive sessions do not leave stale pending-approval state attached to the session after the deterministic deny/failure path completes.
  - Keep emitted tool results and session errors consistent with existing runtime event shapes so CLI and harness consumers do not need a second non-interactive error channel.
- `crates/quine-core/src/channel.rs`
  - Add additive create-session fields only if prompt behavior cannot be derived cleanly from existing callers.
- `crates/quine-harness/src/config.rs`
  - Extend `SessionConfig` only if startup callers need an explicit headless/background discriminator.
- `crates/quine-harness/src/service.rs` and scheduling modules
  - Ensure scheduled/background child sessions are created with deterministic non-interactive prompt behavior.
  - Reuse existing scheduler/session bookkeeping so completion state is observable through current session-listing or context APIs without inventing a scheduler-only permission channel.
- `crates/quine-cli/src/chat.rs`
  - Preserve interactive behavior for REPL sessions.
- CLI one-shot execution path
  - Mark one-shot sessions as non-interactive if they cannot service approval prompts live.
  - Preserve existing output structure, but ensure permission-specific failure text is surfaced clearly in both plain-text and JSON-friendly modes used today.
- Tests in `quine-core`, `quine-harness`, and CLI integration modules
  - Add headless startup and fail-safe prompt-behavior coverage.
  - Prefer tests that verify non-interactive sessions complete with deterministic denial/failure and no leftover pending approval state.

## Validation Plan

- Integration tests for session bootstrap behavior:
  - interactive chat sessions initialize interactive prompt behavior
  - one-shot or batch sessions initialize non-interactive prompt behavior
  - scheduled/background sessions initialize scheduled/background prompt behavior
- Evaluator/approval tests:
  - a would-be `ask` in headless mode resolves to deterministic deny or explicit failure
  - the same request in interactive mode remains eligible for approval routing
  - no path silently allows a prompt-worthy action just because the session is non-interactive
- Daemon-backed or harness-backed tests:
  - a missing responder in a headless/background session produces a visible denied/failure outcome and does not leave pending approval state behind
- CLI/status tests:
  - operator-visible output clearly distinguishes non-interactive permission denial from transport or runtime errors
  - `ps`/context inspection should show completion or explicit failure rather than a session stuck forever waiting on a responder that does not exist
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Re-reviewed `features/plans/050-headless-and-background-permission-behavior-qa.md` after its latest revision.
- The QA plan now matches this implementation plan’s concrete interaction points with existing code:
  - exact session-start distinctions across interactive, one-shot/headless, and scheduled/background flows
  - daemon-backed and CLI-visible checks that `ask` degrades to deterministic denial/failure when no responder exists
  - explicit verification that no stale pending approval remains in inspected session state
- Scope remains aligned: fail-safe non-interactive semantics only, reuse of existing runtime events and inspection surfaces, and remote responders still explicitly out of scope.
- No further QA-side changes are required from this review.
