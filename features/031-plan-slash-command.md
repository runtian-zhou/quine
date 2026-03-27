---
status: pending
---

# `/plan` Slash Command Starts a Plan-Mode Session

## Overview

Add a chat slash command, `/plan`, that starts planning in read-only plan mode when entered from the interactive CLI. Today plan mode already exists as a session-level capability created with `quine chat --plan`, but interactive chat input is forwarded verbatim to the agent and there is no slash-command path that switches the user into plan mode after launch.

This feature should make `/plan` a first-class interactive command that creates a new session with `plan_mode: true`, preserves the existing daemon-backed session architecture, and gives the user a clear visual confirmation that subsequent prompts are being handled in plan mode. The change should build on the existing plan mode implementation in `crates/quine-core/src/engine.rs` and should not change cross-crate trait boundaries.

## Requirements

### 1. Add slash-command handling in the interactive CLI

Update the interactive chat path in `crates/quine-cli/src/chat.rs` to intercept slash commands before sending user input via `SEND_MESSAGE`.

Required command behavior:

- `/plan` with no arguments creates a new session with `plan_mode: true` and makes it the active session for the REPL.
- `/plan <request>` creates a new session with `plan_mode: true`, immediately sends `<request>` as the first message, and streams the result in the same way as a normal chat turn.
- `/plan` must not send the literal string `/plan` to the LLM as a user message.
- If the current session is already a plan-mode session, `/plan` should be a no-op with a small user-facing notice rather than creating duplicate plan sessions.
- Existing `/quit` handling must continue to work unchanged.

Suggested helper shape in `crates/quine-cli/src/chat.rs`:

```rust
async fn create_session(
    client: &mut IpcClient,
    skills: &[String],
    plan_mode: bool,
    auto_approve_permissions: bool,
) -> anyhow::Result<String>;

fn parse_slash_command(input: &str) -> Option<SlashCommand>;

enum SlashCommand {
    Plan { initial_prompt: Option<String> },
    Quit,
}
```

The implementation should prefer extracting existing session creation logic from `run_chat` into a reusable helper instead of duplicating the JSON-RPC request building inline.

### 2. Add matching slash-command support in the TUI

Update `crates/quine-cli/src/tui/mod.rs` and `crates/quine-cli/src/tui/app.rs` so typing `/plan` in the TUI input box has the same semantic effect as in the line-oriented chat REPL.

Required behavior:

- When idle, submitting `/plan` creates a new plan-mode session and updates `App.session_id` to the new session.
- When idle, submitting `/plan <request>` creates a new plan-mode session, updates `App.session_id`, marks `App.plan_mode = true`, and sends `<request>` to the new session.
- If the app is already attached to a plan-mode session, `/plan` should not create another session.
- Existing behavior for regular messages, pending interactions, cancel, and history navigation must remain unchanged.

The TUI currently stores `plan_mode` in `App` as display state and uses it for the input label. This feature should make `App.plan_mode` reflect the active session after a slash-command-triggered session switch.

Suggested helper shape in `crates/quine-cli/src/tui/mod.rs`:

```rust
async fn create_session(
    client: &mut IpcClient,
    skills: &[String],
    plan_mode: bool,
    auto_approve_permissions: bool,
) -> anyhow::Result<(String, Option<u64>)>;
```

If `max_context_window` is returned from `CREATE_SESSION`, preserve the current pattern used in `run_tui_chat`.

### 3. Keep plan mode as session creation, not mid-session mutation

Do not add a new core or harness API for mutating plan mode on an existing session. The current architecture already models plan mode at session creation time:

- `CoreInput::CreateSession` in `crates/quine-core/src/channel.rs`
- `SessionContext::new(..., plan_mode, ...)` in `crates/quine-core/src/engine.rs`
- `SessionConfig.plan_mode` in `crates/quine-harness/src/config.rs`
- `CREATE_SESSION` handling in `crates/quine-harness/src/server.rs`

This feature must continue to use that flow by creating a fresh session in plan mode and switching the CLI/TUI to it.

### 4. Preserve existing plan-mode tool restrictions and prompt behavior

Do not change the core semantics of plan mode already implemented in `crates/quine-core/src/engine.rs`:

- `PLAN_MODE_SYSTEM_PROMPT` remains the source of plan-mode behavioral constraints.
- Plan-mode sessions must continue to register only read-oriented tools (`ReadTool`, `BashTool`, `FindTool`, `AskUserTool`, `PlanTool`) and exclude write/subagent/session-control tools.
- The feature is an entry-point improvement for interactive clients, not a redesign of plan mode itself.

### 5. Show clear session-switch feedback

Both interactive clients should provide a concise user-visible notice when `/plan` creates or switches to a plan-mode session.

Minimum behavior:

- Print or render the new session ID.
- Indicate that plan mode is active.
- Keep the existing `[plan] > ` TUI input label behavior after the switch.

A simple text notice is sufficient; do not add modal UI or new protocol notifications.

### 6. Keep one-shot and non-interactive commands unchanged

Do not change the behavior of:

- `quine run`
- `quine respond`
- `quine chat --plan`

The slash command is for interactive chat/TUI entry only. Existing CLI flags remain supported.

## Acceptance Criteria

- `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check` pass.
- In line-based interactive chat, entering `/plan` creates a new active session with `plan_mode: true` instead of sending `/plan` to the model.
- In line-based interactive chat, entering `/plan How should I implement X?` creates a plan-mode session and immediately sends the planning request.
- In the TUI, entering `/plan` creates a new active plan-mode session and updates the UI state to show plan mode.
- In the TUI, entering `/plan <request>` creates a new active plan-mode session and immediately runs the request.
- Existing `quine chat --plan` behavior remains unchanged.
- Existing normal message sending, permission prompts, and interaction-response handling continue to work.
- Existing plan-mode restrictions in `crates/quine-core/src/engine.rs` remain unchanged.

Specific tests required:

- Unit tests in `crates/quine-cli/src/chat.rs` for slash-command parsing:
  - parses `/plan`
  - parses `/plan <request>`
  - ignores non-command input
  - preserves `/quit`
- Unit tests in `crates/quine-cli/src/tui/app.rs` or adjacent TUI module for submit behavior when the input contains `/plan`.
- Integration-style tests in `crates/quine-cli` or `crates/quine-harness` covering:
  - interactive session creation request includes `plan_mode: true` for `/plan`
  - `/plan` does not send the literal slash command to `SEND_MESSAGE`
  - `/plan <request>` creates a new session and sends only the request payload
- Existing tests must continue to pass.

## QA Test Cases (add to `qa/test_cases.json`)

```json
[
  {
    "name": "slash_plan_from_chat_repl",
    "description": "Typing /plan in interactive chat creates a new plan-mode session",
    "steps": [
      "Start `quine chat`",
      "Type `/plan`",
      "Verify the CLI reports a new session ID and indicates plan mode is active",
      "Send a planning request in the new session",
      "Verify the agent uses read-only exploration and returns a plan"
    ]
  },
  {
    "name": "slash_plan_with_inline_request",
    "description": "Typing /plan with an inline request creates a plan session and immediately runs the request",
    "steps": [
      "Start `quine chat`",
      "Type `/plan How would I add a new tool?`",
      "Verify the active session switches to a new plan-mode session",
      "Verify the model receives only `How would I add a new tool?` as the user request",
      "Verify the response is a plan, not an implementation"
    ]
  },
  {
    "name": "slash_plan_from_tui",
    "description": "Typing /plan in the TUI switches the app to a new plan-mode session",
    "steps": [
      "Start `quine chat` in a terminal that uses the TUI",
      "Enter `/plan` and submit",
      "Verify the app remains usable and the input label shows `[plan] >`",
      "Verify subsequent prompts execute in plan mode"
    ]
  }
]
```

## Non-Goals (Deferred)

- Switching an existing session in-place from normal mode to plan mode inside `quine-core`
- Adding new JSON-RPC methods specifically for plan-mode switching
- General-purpose slash-command framework for arbitrary commands beyond `/plan` and existing `/quit`
- Automatic return from plan mode to implementation mode
- Reusing prior conversation history when `/plan` creates the new session unless that behavior already exists explicitly
