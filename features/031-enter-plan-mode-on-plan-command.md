---
status: done
---

# Enter Plan Mode on `/plan` Command

## Overview

Add first-class slash-command handling for `/plan` so that entering `/plan <request>` starts the feature-request workflow in read-only plan mode instead of sending the literal text to the agent. This makes the CLI behave more like Claude Code command prompts while reusing Quine's existing skill discovery and plan-mode session support.

The implementation should recognize `/plan` as a local client command in both the REPL and TUI flows. When invoked, Quine should create or switch to a plan-mode session and dispatch the remainder of the user input as the first planning request. The command should work with existing legacy command files under `.claude/commands/` and should not change inter-crate trait contracts.

## Requirements

### 1. Slash Command Parsing in `crates/quine-cli/src/chat.rs` and `crates/quine-cli/src/tui/`

Introduce shared parsing logic for leading slash commands entered by the user in interactive chat surfaces.

Required behavior:
- Detect inputs beginning with `/` before sending them via `SEND_MESSAGE`.
- Parse `/plan` with optional trailing arguments.
- Preserve existing `/quit` behavior in the REPL.
- Non-command input must continue through the normal message path unchanged.
- Unknown slash commands should surface a user-visible error instead of being silently sent as agent text.

A concrete shape that fits current code is:

```rust
struct SlashCommand {
    name: String,
    arguments: String,
}

fn parse_slash_command(input: &str) -> Option<SlashCommand>;
```

Keep the parsing implementation crate-private within `quine-cli` and reuse it from both `chat.rs` and `tui/mod.rs` rather than duplicating string handling.

### 2. `/plan` Must Start a Plan-Mode Session

When the user enters `/plan`:
- Create a session with `plan_mode: true` if the active chat surface is not already backed by a plan-mode session.
- Send the remaining arguments as the first user message when non-empty.
- If no arguments are supplied, show a prompt or guidance asking what should be planned rather than dispatching an empty message.
- The user-visible prompt label should reflect plan mode consistently with the existing `plan_mode` UI indicators in `crates/quine-cli/src/tui/app.rs` and `crates/quine-cli/src/tui/ui.rs`.

Because current session mode is fixed at session creation time, the client will likely need a local helper that creates a replacement session and updates the in-memory `session_id` used for subsequent requests.

### 3. Direct Plan-Mode Entry Without Legacy Command Injection

Quine already loads legacy Claude command files from `.claude/commands/` in `crates/quine-core/src/skill.rs`, and that compatibility must remain intact for normal slash-skill sessions. However, `/plan` itself should enter plan mode directly rather than depending on `.claude/commands/feature-request.md` or any dedicated command markdown file.

Expected flow:
- Recognize `/plan` locally in the interactive client.
- Create or switch to a session with `plan_mode: true`.
- Rely on the existing plan-mode system prompt composition in `crates/quine-core/src/engine.rs` so the resulting agent behavior stays read-only and produces a plan.
- Preserve legacy `.claude/commands/` loading behavior for actual slash-skill commands such as `/review`.

A dedicated `.claude/commands/plan.md` is not required for this feature because the intended behavior is mode switching, not command-prompt injection.

### 4. Command-to-Session Wiring Across IPC Boundaries

If additional session creation parameters are needed to support `/plan`, update only the existing concrete session creation payloads and keep crate-boundary traits unchanged.

Relevant files to inspect and reuse:
- `crates/quine-cli/src/chat.rs`
- `crates/quine-cli/src/tui/mod.rs`
- `crates/quine-harness/src/protocol.rs`
- `crates/quine-harness/src/server.rs`
- `crates/quine-harness/src/local.rs`
- `crates/quine-core/src/channel.rs`

The implementation should prefer existing `system_prompt`, `skills`, and `plan_mode` session fields over adding new ad hoc fields unless a clear gap is identified.

### 5. Tests

Add focused tests near the affected code:
- Unit tests for slash-command parsing in the same module as the parser.
- Tests that `/plan foo` results in plan-mode session configuration.
- Tests that `/plan` without arguments does not send an empty message.
- TUI or app-level tests covering visible plan-mode state after the command is handled, where existing patterns allow.
- Existing skill loader behavior for legacy `.claude/commands/` files must continue to pass.

## Acceptance Criteria

- `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --all -- --check` all pass.
- Entering `/plan Add support for X` in the REPL creates or switches to a plan-mode session and sends `Add support for X` as the first planning request.
- Entering `/plan` without arguments produces a clear client-side prompt for more detail and does not send an empty message.
- Unknown slash commands are reported locally to the user.
- Existing non-command chat behavior remains unchanged.
- Legacy `.claude/commands/` compatibility remains intact.
- Required unit tests cover slash parsing and `/plan` session behavior.
- Existing tests continue to pass.

## QA Test Cases

- Start `cargo run --bin quine -- chat`, enter `/plan Enter plan mode if enter /plan`, and verify the created session runs in plan mode and produces a planning-oriented response rather than treating the slash command as plain text.
- Start the TUI chat flow, enter `/plan audit the slash command system`, and verify plan-mode indicators are active for the resulting session.
- Enter `/plan` with no trailing text and verify the client asks for details instead of sending a blank request.
- Enter an unknown command such as `/does-not-exist` and verify a local error is shown.
- Confirm normal text prompts and `/quit` still behave as before.

## Non-Goals (Deferred)

- Full support for arbitrary slash commands beyond `/plan` and existing `/quit`.
- Mid-turn switching between normal and plan mode.
- Server-side slash-command parsing for non-interactive callers.
- Automatic execution of the resulting implementation plan.
