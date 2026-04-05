# TUI Slash Command Prompter and `ps` Command

Short summary: validate that the TUI exposes a usable slash-command prompter in the composer and that the `quine ps` CLI command accurately reports active sessions in plain, tree, and JSON formats.

## Open Questions

- None currently. The existing daemon session-listing path appears sufficient for QA coverage.

## Agreement Status

- Status: agreed
- Implementation plan review: reviewed latest implementation doc revision and confirmed file-level design/test coverage matches the QA scenarios.
- Notes: no unresolved open questions; QA plan is aligned with the implementation plan.

## Test Strategy

- Prefer black-box validation through the real local daemon and CLI/TUI entrypoints.
- Cover both no-session and active-session cases for `ps`.
- Cover slash prompter discovery, navigation, insertion, and execution behavior in the TUI so the prompt affects the actual composer path rather than a mocked overlay.
- Supplement manual daemon-backed checks with focused unit/integration tests where the implementation introduces isolated filtering or formatting helpers.

## Scenarios

- **Scenario 1: `ps` with no active sessions**
  - Start the local daemon: `cargo run --bin quine-harness -- daemon`
  - In a separate shell, run: `cargo run --bin quine -- ps`
  - Expected result: command exits successfully and prints an empty-state message or empty table with no session rows; it must not crash.
  - Then run: `cargo run --bin quine -- ps --json`
  - Expected result: successful JSON output representing zero sessions, such as `[]` or an equivalent empty response structure defined by the implementation.

- **Scenario 2: `ps` reports an interactive session**
  - Start the local daemon: `cargo run --bin quine-harness -- daemon`
  - In a second shell, start a chat session and keep it open: `cargo run --bin quine -- chat`
  - Send round 1 message: `hello`
  - Expected chat result: the session stays open and the CLI displays a normal assistant response with no transport error.
  - In a third shell, run: `cargo run --bin quine -- ps`
  - Expected result: successful output that includes at least one active session row with a session id and enough metadata to identify the running chat session.
  - Then run: `cargo run --bin quine -- ps --tree`
  - Expected result: successful hierarchical output containing the same active session id in tree form.
  - Then run: `cargo run --bin quine -- ps --json`
  - Expected result: valid JSON containing an item for the active session with fields matching the harness session-list response.

- **Scenario 3: `ps` updates after session exit**
  - With the daemon running, start and then exit `cargo run --bin quine -- chat`.
  - After the chat process fully exits, run: `cargo run --bin quine -- ps`
  - Expected result: the previously active session is absent or marked non-active according to the implementation contract; output remains consistent across repeated invocations.

- **Scenario 4: TUI slash prompter opens and inserts a command**
  - Start the local daemon: `cargo run --bin quine-harness -- daemon`
  - Launch the TUI: `cargo run --bin quine -- chat`
  - In the composer, type exactly: `/`
  - Expected result: a slash-command prompter appears near the composer listing supported slash commands.
  - Press the down-arrow key until a non-default command is highlighted, then press `Enter` or the implemented accept key.
  - Expected result: the prompter closes and the selected slash command text is inserted into the composer in the expected canonical form, ready for argument editing or submission.

- **Scenario 5: TUI slash prompter filters and executes a command**
  - Start the local daemon: `cargo run --bin quine-harness -- daemon`
  - Launch the TUI: `cargo run --bin quine -- chat`
  - In the composer, type a prefix such as `/he`.
  - Expected result: the prompter narrows to matching slash commands only.
  - Accept the intended command from the prompt, then complete any required arguments and submit.
  - Expected result: the command follows the same execution path as a manually typed slash command; the UI shows the same status text, assistant output, and any side effect expected for that command.

- **Scenario 6: TUI slash prompter dismissal and normal chat input**
  - Start the local daemon: `cargo run --bin quine-harness -- daemon`
  - Launch the TUI: `cargo run --bin quine -- chat`
  - Type `/` to open the prompter, then press `Esc`.
  - Expected result: the prompter closes without corrupting the current input buffer.
  - Clear the composer, type `hello`, and submit.
  - Expected result: the message is treated as a normal chat turn, with standard assistant output and no slash-command handling.

## Required Evidence

- Terminal transcript or captured output for:
  - `cargo run --bin quine -- ps`
  - `cargo run --bin quine -- ps --tree`
  - `cargo run --bin quine -- ps --json`
- Evidence that at least one active session appears while `chat` is running and disappears or changes appropriately after exit.
- TUI capture notes or screenshots showing:
  - slash prompter visible after typing `/`
  - filtered results after typing a command prefix
  - composer contents after accepting a suggestion
- Automated test evidence from relevant focused tests plus `cargo test -p quine-cli`.
- CI-style evidence from `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all -- --check`.

## Implementation Feedback

- Implementation plan identifies the correct `quine-cli` touch points: TUI app/input state, TUI rendering, slash-command metadata/helpers, and `main.rs`/session-list rendering for `ps`.
- Validation scope is sufficient: focused tests, `cargo test -p quine-cli`, clippy, fmt, and manual daemon-backed checks cover the expected risk areas.
