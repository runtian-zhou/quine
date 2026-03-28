# 031 Enter Plan Mode on `/plan` Command — QA Plan

## Ownership

- Owner: QA agent
- Status: revised

## Test Objectives

Verify that slash-command parsing is handled locally in interactive chat surfaces, that `/plan` reliably activates or switches to a plan-mode session without sending empty requests, that `/plan` enters plan mode directly without depending on `.claude/commands/feature-request.md`, and that existing `/quit` plus normal chat behavior remain unchanged.

## Planned Coverage

1. Parser unit tests for recognized, unrecognized, and non-command inputs.
2. REPL behavior checks for `/plan <request>`, bare `/plan`, unknown slash commands, and `/quit`.
3. TUI behavior checks at the `App::submit_input` / action / state-transition seam, with only light rendering assertions for plan-mode label or inline errors where current patterns support them.
4. Regression checks for unchanged normal chat behavior and any unrelated legacy command loading behavior that already exists.
5. Session creation checks proving `/plan` results in `plan_mode: true`, replacement-session handling when needed, and direct plan-mode entry without injecting legacy `feature-request` command content.
6. Workspace validation via `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check`.

## Concrete QA Strategy

### 1. Parser seam

- Prefer unit tests around a crate-private slash parser helper rather than only end-to-end assertions.
- Required parser cases:
  - `/plan do thing` parses as command `plan` with arguments `do thing`
  - `/plan` parses as command `plan` with empty arguments
  - `/quit` parses distinctly and preserves current REPL semantics
  - `/does-not-exist` parses as an unknown slash command for local rejection
  - `hello` and whitespace-only input remain non-command input

### 2. TUI seam

- The cleanest validation point is `crates/quine-cli/src/tui/app.rs` because `App::submit_input` already turns user input into `AppAction`s before RPC execution in `crates/quine-cli/src/tui/mod.rs`.
- QA expects tests to prove:
  - `/plan <request>` does not just become `AppAction::SendMessage("/plan ...")`
  - bare `/plan` does not produce a send action
  - unknown commands create a local error entry, ideally `ConversationEntry::Error`
  - successful `/plan` handling updates visible plan-mode state through `App.plan_mode` and `input_label()`

### 3. REPL seam

- `crates/quine-cli/src/chat.rs` currently checks only hardcoded `/quit` before `SEND_MESSAGE`; `/plan` must be intercepted at the same pre-send seam.
- QA expects tests or focused integration coverage proving:
  - `/plan <request>` creates or switches to a plan-mode session before any `SEND_MESSAGE`
  - the sent content is only the trailing request text, not the literal slash command
  - bare `/plan` emits local guidance and never sends an empty `content`
  - unknown slash commands are surfaced locally rather than forwarded to the daemon

### 4. Session and prompt wiring

- `crates/quine-harness/src/server.rs` already accepts `skills`, `plan_mode`, and `system_prompt` in `create_session`; QA does not expect trait changes.
- `crates/quine-core/src/engine.rs` already composes the read-only `PLAN_MODE_SYSTEM_PROMPT` at session creation time, so QA wants explicit proof that `/plan` creates a new plan-mode session when starting from normal chat.
- Because `crates/quine-core/src/skill.rs` already loads legacy `.claude/commands/*.md`, QA should still avoid regressions there, but `/plan` should not depend on `.claude/commands/feature-request.md`. Validation should instead prove that entering `/plan` directly triggers existing plan-mode behavior.

## Risk Notes

- **Late interception risk**: If slash parsing happens after `SEND_MESSAGE`/`AppAction::SendMessage`, `/plan` may leak to the agent as plain text.
- **Session replacement risk**: REPL `session_id` and TUI `session_id` plus `plan_mode` must be updated together when switching to a replacement plan-mode session.
- **Empty-command risk**: bare `/plan` may accidentally add a user transcript entry or send blank content across IPC.
- **Prompt-source risk**: the implementation could accidentally inject legacy `feature-request` command content even though `/plan` is now intended to switch into plan mode directly.
- **Regression risk**: `/quit` in REPL and non-command input in both chat surfaces must remain unchanged.

## Validation Targets

- `crates/quine-cli/src/chat.rs`
- `crates/quine-cli/src/tui/app.rs`
- `crates/quine-cli/src/tui/mod.rs`
- `crates/quine-harness/src/server.rs`
- `crates/quine-harness/src/local.rs`
- `crates/quine-core/src/engine.rs`
- `crates/quine-core/src/skill.rs`

## Suggested Manual Scenarios

- Run `cargo run --bin quine -- chat`, enter `/plan Enter plan mode if enter /plan`, and confirm the session behaves as plan mode.
- Run the TUI, enter `/plan audit the slash command system`, and confirm plan-mode labeling becomes active.
- Enter bare `/plan` and confirm the client requests more detail instead of sending an empty message.
- Enter `/does-not-exist` and confirm a local error is shown.
- Enter normal text and `/quit` to confirm no regression.

## Required Evidence

- Unit tests covering slash-command parsing cases.
- Focused tests showing `/plan` toggles or replaces session state into `plan_mode: true`.
- Focused tests showing bare `/plan` does not cross the IPC boundary.
- Evidence that `/plan` does not depend on `.claude/commands/feature-request.md` and instead enters plan mode directly.
- Passing workspace checks: `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check`.

## Open Questions

- None at this point. Current code inspection answers the earlier QA questions:
  - TUI-local command handling is best tested at `App::submit_input` and related action/state transitions, not pixel-level rendering.
  - Plan-mode verification should assert session configuration and that `/plan` enters plan mode directly, without requiring legacy command content injection.
  - The cleanest seam for proving bare `/plan` does not cross IPC is to keep command handling before `AppAction::SendMessage` / `SEND_MESSAGE` dispatch and assert no send action occurs.

## Review Of Implementation Plan

The implementation plan is aligned with QA expectations after code inspection. The key points QA agrees with are:

- Reuse of a shared crate-private slash-command helper rather than duplicating parsing in REPL and TUI.
- Treating `crates/quine-cli/src/tui/app.rs` as the primary TUI test seam.
- Reusing existing `create_session` fields (`skills`, `plan_mode`, `system_prompt`) instead of changing crate-boundary traits.
- Keeping `/plan` as a direct plan-mode entry point rather than routing it through `.claude/commands/feature-request.md`.

QA specifically requests that the final implementation include at least one assertion proving direct plan-mode entry and no-send behavior for bare `/plan`.

## Agreement Log

- Reviewed the feature request, implementation plan, and relevant CLI/harness/core code paths.
- Agreement item: QA agrees the TUI should report local command failures through `ConversationEntry::Error` rather than a separate status channel.
- Agreement item: User direction clarifies that `/plan` should enter plan mode directly and must not reuse `.claude/commands/feature-request.md`.
- Agreement item: No remaining QA-side open questions; implementation plan is agreed provided the final tests cover direct plan-mode entry and no-send behavior for bare `/plan`.
