# TUI Slash Command Prompter and `ps` Command

Short summary: add an interactive slash-command prompter to the TUI chat input flow and implement the `quine ps` command so active sessions can be inspected from the CLI.

## Open Questions

- None at the moment. Current research suggests both changes fit existing `quine-cli` and harness session-listing interfaces without changing cross-crate traits.

## Agreement Status

- Status: agreed
- QA plan review: reviewed latest QA doc revision and aligned on daemon-backed `ps` coverage plus interactive TUI slash-prompt scenarios.
- Notes: no unresolved open questions; implementation and QA plans are aligned on scope, file touch points, and validation expectations.

## Proposed Design

- Add TUI-side slash-command discovery and prompt state on top of the existing `TextArea` input in `crates/quine-cli/src/tui/app.rs` and rendering in `crates/quine-cli/src/tui/ui.rs`.
- Reuse `crates/quine-cli/src/slash_command.rs` as the canonical parser/executor boundary, but extend the TUI flow so typing `/` can surface known commands and allow selection/completion before submit.
- Keep execution semantics centralized: once a slash command is confirmed, route through the existing slash command handling used by TUI submit behavior rather than introducing a parallel execution path.
- Implement `Commands::Ps` in `crates/quine-cli/src/main.rs` by calling the existing session-listing client path (`SessionClient::list_sessions`) and rendering either JSON, a flat table, or a tree view depending on flags.
- Preserve current CLI and harness trait boundaries: no changes to core orchestration traits; only consume already-exposed daemon/session APIs from `quine-cli`.

## File-by-File Changes

- `crates/quine-cli/src/tui/app.rs`: add slash-prompt state, filtered command list generation, key handling for opening/navigating/accepting the prompter, and submit integration.
- `crates/quine-cli/src/tui/ui.rs`: render the slash-command prompter near the composer, including selected item highlighting and any short help text.
- `crates/quine-cli/src/slash_command.rs`: if needed, expose lightweight metadata/helpers so the TUI can list supported commands and completion text without duplicating command definitions.
- `crates/quine-cli/src/main.rs`: implement the `Ps` subcommand end-to-end, including table/tree formatting and JSON output.
- `crates/quine-cli/src/session.rs`: reuse existing `list_sessions` plumbing; only update if a small helper is needed for presentation.
- `crates/quine-cli/src/chat.rs` or adjacent shared display utilities: optionally factor shared session-formatting helpers if needed to avoid duplicate rendering logic between `log --list` and `ps`.
- `crates/quine-cli` tests near touched modules: add unit tests for slash prompter filtering/completion helpers and `ps` output selection/formatting where existing patterns support it.

## Validation Plan

- Run focused `quine-cli` tests covering slash command parsing/prompt state and `ps` presentation helpers.
- Run `cargo test -p quine-cli`.
- Run `cargo clippy --all-targets -- -D warnings`.
- Run `cargo fmt --all -- --check`.
- Manually verify the TUI flow by launching `cargo run --bin quine -- chat`, typing `/`, navigating the prompt, confirming a command, and ensuring normal message submission still works.
- Manually verify `cargo run --bin quine -- ps`, `cargo run --bin quine -- ps --tree`, and `cargo run --bin quine -- ps --json` against a local daemon with at least one active session.

## QA Feedback

- QA requested concrete daemon-backed validation for `ps` empty/active/exited-session states and interactive TUI checks for open, filter, accept, dismiss, and execute flows.
- The implementation plan already supports those scenarios through `main.rs`, `session.rs`, and TUI prompt state/rendering changes, so no further plan changes are required.
