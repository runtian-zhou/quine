# Bounded Bash Output Box In TUI

Short summary: Verify that bash execution stdout shown in the TUI is rendered inside a bounded box so long command output does not consume the full visible screen and key status information remains readable.

## Open Questions

- None at this stage.

## Agreement Status

- Status: agreed
- Last reviewed implementation plan revision: aligned with the latest implementation scope that keeps the feature inside `quine-cli` TUI rendering and preserves existing tool-result data flow.

## Test Strategy

- Focus QA on `quine-cli` TUI behavior with real command output lengths that previously risked overflowing the visible transcript.
- Combine automated crate validation with at least one real local daemon / CLI interaction that exercises the bash tool path end-to-end.
- Validate both success and failure-oriented command output presentation so the box treatment does not hide execution status.
- Confirm unaffected tools and regular assistant text continue to render normally around the boxed output.

## Scenarios

- Scenario 1: Automated regression checks for TUI crate
  - Start/use local environment: no daemon required.
  - Exact commands:
    - `cargo test -p quine-cli`
    - `cargo clippy -p quine-cli --all-targets -- -D warnings`
    - `cargo fmt --all -- --check`
  - Expected result:
    - All tests pass.
    - Clippy reports no warnings.
    - Format check passes.

- Scenario 2: One-off local daemon chat with long stdout
  - Start the local daemon in one terminal:
    - `cargo run --bin quine-harness`
  - Connect from another terminal with the interactive CLI:
    - `cargo run --bin quine -- chat`
  - Exact chat messages to send, round by round:
    - Round 1 message: `Run a bash command that prints the numbers 1 through 120, then summarize what happened in one sentence.`
  - Expected response for round 1:
    - Tool activity shows a bash tool invocation.
    - The TUI renders the bash stdout preview inside a visually bounded box rather than as unbounded transcript lines.
    - The visible box height stays limited; the full screen is not replaced by output lines.
    - A final assistant response appears after tool execution with a one-sentence summary.
    - Any status text still indicates the command completed successfully.

- Scenario 3: One-off local daemon chat with wide and multiline stdout
  - Start the local daemon:
    - `cargo run --bin quine-harness`
  - Connect via CLI:
    - `cargo run --bin quine -- chat`
  - Exact chat messages to send, round by round:
    - Round 1 message: `Run a bash command that prints 10 lines of 200 x characters each, then tell me whether the output was truncated in the preview.`
  - Expected response for round 1:
    - Tool activity shows the bash invocation.
    - The stdout preview remains inside the same bounded box style.
    - Long lines wrap or clip within the box width instead of overflowing the conversation pane.
    - The preview remains height-limited.
    - The final assistant response remains visible below or after the tool result.

- Scenario 4: One-off local daemon chat with failing command stderr/status visibility
  - Start the local daemon:
    - `cargo run --bin quine-harness`
  - Connect via CLI:
    - `cargo run --bin quine -- chat`
  - Exact chat messages to send, round by round:
    - Round 1 message: `Run a bash command that prints 40 lines and exits with a non-zero status, then explain the failure.`
  - Expected response for round 1:
    - Tool activity shows a bash invocation.
    - Output preview still appears in the bounded box if stdout is present.
    - Failure state, error text, or non-zero status remains visible and is not obscured by the box.
    - The final assistant response explains the command failed.

## Required Evidence

- Terminal transcript or captured notes showing the exact commands used for each manual scenario.
- Confirmation that the bounded box appears for long bash stdout.
- Confirmation that visible output height stays bounded and the rest of the conversation remains on screen.
- Confirmation that success/failure status text remains visible.
- Results of `cargo test -p quine-cli`, `cargo clippy -p quine-cli --all-targets -- -D warnings`, and `cargo fmt --all -- --check`.

## Implementation Feedback

- The implementation plan keeps the change appropriately scoped to `crates/quine-cli/src/tui/ui.rs`, which matches the current rendering hotspot for tool output previews.
- Reusing the existing `result_preview` flow is the lowest-risk approach and avoids unnecessary changes to cross-crate interfaces or daemon transport.
- The plan should preserve visible success/failure metadata while bounding stdout height and width, which matches the acceptance criteria for avoiding screen-flushing output.
- The automated and manual QA scenarios are sufficient to validate the proposed design.
