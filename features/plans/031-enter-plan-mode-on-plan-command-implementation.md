# Enter Plan Mode on `/plan` Command — Implementation Plan

Feature request: `features/031-enter-plan-mode-on-plan-command.md`

Summary: Add client-side `/plan` slash-command handling so interactive chat surfaces switch into plan mode and dispatch the remainder of the input as the first planning request while preserving existing skill-loading compatibility.

## Open Questions

- Should `/plan` load a new dedicated command prompt file such as `.claude/commands/plan.md`, or should it map to the existing `feature-request` legacy command prompt plus plan mode restrictions?
- When `/plan` is entered in an existing non-plan session, should the old session remain alive and merely stop receiving input, or should the client explicitly communicate that a new plan session has replaced it?

## Agreement Status

Status: pending

## Proposed Design

- Add a crate-private slash-command parser in `quine-cli` that extracts command name and trailing arguments.
- Route REPL and TUI submission paths through a shared command handler before normal `SEND_MESSAGE` dispatch.
- Implement `/plan` as a local client command that creates a plan-mode session using the existing session-creation payload fields: `plan_mode`, `skills`, and optionally `system_prompt` if needed.
- Resolve the command prompt through Quine's legacy command-loading mechanism so the behavior stays aligned with `.claude/commands/` compatibility.
- Update client state so subsequent turns use the new plan-mode session and visual indicators remain accurate.

## File-by-File Changes

- `crates/quine-cli/src/chat.rs`: intercept line input, parse slash commands, preserve `/quit`, and create/swap sessions for `/plan` before sending the first planning message.
- `crates/quine-cli/src/tui/mod.rs`: route submitted text through the same slash-command handling path and update in-memory app/session state after switching to a plan-mode session.
- `crates/quine-cli/src/tui/app.rs`: confirm app state can reflect a session transition into plan mode; add small helpers if needed.
- `crates/quine-cli/src/...` new helper module if warranted: hold the shared slash parser plus local command execution helpers used by both REPL and TUI.
- `crates/quine-core/src/skill.rs` or adjacent callers only if needed to reliably load the legacy command prompt by command name without changing crate-boundary traits.
- `.claude/commands/plan.md` only if the implementation decides a dedicated command prompt is cleaner than reusing `feature-request.md`.

## Validation Plan

- Add parser unit tests covering `/plan`, `/plan   foo`, plain text, `/quit`, and unknown commands.
- Add client-level tests for `/plan` with and without arguments.
- Run `cargo test -p quine-cli` first, then full workspace checks required by the feature request.

## QA Feedback

Pending QA review.
