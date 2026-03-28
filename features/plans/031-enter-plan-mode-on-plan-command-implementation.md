# 031 Enter Plan Mode on `/plan` Command — Implementation Plan

## Ownership

- Owner: implementor agent
- Status: draft

## Goal

Add client-side slash-command parsing so `/plan <request>` creates or switches to a plan-mode session and submits `<request>` as the first planning message, while preserving existing non-command behavior and legacy command compatibility.

## Proposed Approach

1. Add shared crate-private slash-command parsing in `crates/quine-cli` and reuse it from both REPL and TUI flows.
2. Preserve existing `/quit` behavior and surface a local error for unknown slash commands.
3. Route `/plan` through existing session creation fields, enabling `plan_mode: true` directly without depending on `.claude/commands/feature-request.md`.
4. Avoid sending empty messages for bare `/plan`; instead show local guidance.
5. Add focused tests for parsing, session wiring, and visible plan-mode state where current test patterns support it.

## Files To Inspect

- `crates/quine-cli/src/chat.rs`
- `crates/quine-cli/src/tui/mod.rs`
- `crates/quine-cli/src/tui/app.rs`
- `crates/quine-cli/src/tui/ui.rs`
- `crates/quine-harness/src/protocol.rs`
- `crates/quine-harness/src/server.rs`
- `crates/quine-harness/src/local.rs`
- `crates/quine-core/src/channel.rs`
- `crates/quine-core/src/skill.rs`
- `crates/quine-core/src/engine.rs`

## Concrete Implementation Notes

- `crates/quine-cli/src/chat.rs` currently handles only a hardcoded `/quit` check before every `send_message` call; `/plan` should be intercepted in this same pre-send path rather than added server-side.
- `crates/quine-cli/src/tui/app.rs` currently turns every non-empty input into `AppAction::SendMessage` inside `App::submit_input`; this is the cleanest seam for crate-private slash parsing so `crates/quine-cli/src/tui/mod.rs` can stay focused on RPC execution.
- Both interactive surfaces already create sessions locally through `methods::CREATE_SESSION` with `skills`, `plan_mode`, and `auto_approve_permissions`, so `/plan` can reuse the existing payload without changing inter-crate traits.
- Session mode is fixed at creation time in practice: the CLI stores a single in-memory `session_id`, the TUI stores `session_id` plus `plan_mode` in `App`, and the core composes the read-only architect prompt only when the session is created in `crates/quine-core/src/engine.rs`.
- Legacy command compatibility already exists via `FileSystemSkillLoader::default_paths` in `crates/quine-core/src/skill.rs`, but `/plan` should not depend on `.claude/commands/feature-request.md`; entering plan mode should rely on existing plan-mode session behavior instead.
- The most likely implementation shape is a crate-private slash-command helper in `quine-cli` plus a small session-creation helper that can return a replacement `session_id` for REPL and `(session_id, plan_mode)` state for TUI.

## Open Questions

- `/plan` should enter plan mode directly rather than resolving through `.claude/commands/feature-request.md`. A dedicated `.claude/commands/plan.md` is unnecessary unless future UX work needs extra plan-specific prompt text beyond existing plan-mode behavior.
- TUI-local command errors should render inline in the transcript as `ConversationEntry::Error`, because that matches the existing error surface and is already covered by UI rendering paths. A separate status-only path is not necessary for the first implementation.
- If the active session is not already in plan mode, the client should create a replacement plan-mode session on `/plan`; if it is already in plan mode, it should reuse the current session. This matches the requirement that current session mode is fixed at session creation time.

## Review Of QA Plan

- QA direction looks aligned and the request for testable local-command seams is correct; slash parsing should not be buried directly inside event-loop branches.
- For TUI coverage, existing tests should prefer `App::submit_input` and related action/state transitions over pixel-precise rendering assertions. Rendering checks can stay limited to plan-mode label/error visibility where current patterns already exist.
- Plan-mode verification should assert the client-side session configuration (`plan_mode: true` and replacement session usage) and confirm `/plan` enters plan mode without injecting legacy `feature-request` command content.
- The cleanest seam for proving bare `/plan` does not cross the IPC boundary is to keep slash parsing before `AppAction::SendMessage`/RPC dispatch, then assert that the bare command yields local guidance instead of a send action.

## Agreement Log

- Reviewed the QA plan and agree with its emphasis on parser-level seams, REPL/TUI command handling, and regression coverage for existing command behavior.
- User direction clarified the intended behavior: `/plan` must enter plan mode directly and must not reuse `.claude/commands/feature-request.md`.
