# Auto-approve all operations

Short summary: verify that `--auto-approve` causes every approval-gated operation in the chat flow to be automatically approved, rather than ignored.

## Open Questions

- None.

## Agreement Status

- agreed — reviewed the latest implementation plan revision. Both docs align on a CLI-scoped fix that makes `--auto-approve` answer approval requests in the existing event loop(s) without changing core approval traits.

## Test Strategy

- Cover the smallest deterministic layer first with focused automated tests around approval-request handling in the CLI chat event loop and, if shared, the non-interactive `run` loop.
- Validate one-off CLI behavior with a real local harness daemon so the test proves the session no longer blocks on approval.
- Verify both positive behavior (`--auto-approve` approves automatically) and control behavior (without the flag, approval still requires explicit user action or remains gated as designed).
- Verify the command help text regression so `--auto-approve` no longer advertises itself as ineffective.

## Scenarios

- Automated regression test: CLI approval event auto-response
  - Start/use local daemon: not required if covered with a unit/integration test at the CLI layer.
  - Exact execution: run the targeted test command for the added/updated CLI approval test, e.g. `cargo test -p quine-cli auto_approve -- --nocapture` or the exact test name added by implementation.
  - Expected result: the test simulates an approval request event, verifies the CLI sends an approval response when `auto_approve=true`, and verifies no approval response is auto-sent when `auto_approve=false`.

- Help text regression: flag description reflects real behavior
  - Start/use local daemon: not required.
  - Exact command: `cargo run --bin quine -- chat --help`
  - Expected result: output lists `--auto-approve` and does not say it is a no-op or that it currently has no effect.

- One-off daemon-backed run: approval-gated operation with auto-approve enabled
  - Start local daemon: `cargo run --bin quine-harness -- start --socket /tmp/quine-auto-approve.sock`
  - Exact command: `cargo run --bin quine -- run --socket /tmp/quine-auto-approve.sock --auto-approve "<PROMPT THAT TRIGGERS A KNOWN APPROVAL-GATED OPERATION>"`
  - Exact prompt/message: implementation should provide a deterministic prompt tied to an existing approval-gated tool or fixture before execution.
  - Expected result: the operation runs without interactive approval input; output shows successful tool activity and a final assistant response rather than hanging on an approval prompt.

- Control scenario: same approval-gated run without auto-approve
  - Start/use local daemon: same daemon as prior scenario.
  - Exact command: `cargo run --bin quine -- run --socket /tmp/quine-auto-approve.sock "<SAME PROMPT>"`
  - Exact prompt/message: same approval-gated prompt as above.
  - Expected result: output shows the usual approval prompt, blocked state, or explicit pending approval behavior, proving the new behavior is gated strictly by the flag.

- Interactive chat scenario if implementation lands only in `chat`
  - Start local daemon: `cargo run --bin quine-harness -- start --socket /tmp/quine-auto-approve.sock`
  - Exact command: `cargo run --bin quine -- chat --socket /tmp/quine-auto-approve.sock --auto-approve`
  - Exact messages:
    - Round 1: send `<PROMPT THAT TRIGGERS A KNOWN APPROVAL-GATED OPERATION>`.
    - Round 2: send `Did that complete successfully?`
  - Expected result:
    - Round 1: status/tool output shows the approval-gated operation executes without waiting for manual approval.
    - Round 2: assistant confirms the earlier operation completed successfully and the session remains usable.

## Required Evidence

- Targeted automated test output covering auto-approve approval handling.
- Command transcript or captured output for one end-to-end approval-gated run with `--auto-approve` enabled.
- Command transcript or captured output for the control run without `--auto-approve`.
- If applicable, transcript for the required multi-round daemon-backed scenario.
- Confirmation that `cargo fmt --all -- --check` and relevant tests pass for touched code.

## Implementation Feedback

- The implementation plan now identifies `crates/quine-cli/src/chat.rs` as the primary fix site and asks implementer to verify whether `crates/quine-cli/src/run.rs` shares the same approval event handling path; that is enough for QA agreement on scope.
- Please document the exact deterministic approval-gated prompt or test fixture in the implementation PR so the daemon-backed scenarios can be executed verbatim.
