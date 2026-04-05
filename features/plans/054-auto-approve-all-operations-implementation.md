# Auto-approve all operations

Short summary: make `--auto-approve` actually approve every operation that currently routes through the approval system, instead of being parsed by the CLI and then ignored.

## Open Questions

- None.

## Agreement Status

- agreed — reviewed the QA plan revision that targets the `quine chat` / `quine run` client paths, uses the harness daemon socket flow, and validates both auto-approved and control behavior. The two docs now match on scope and validation.

## Proposed Design

- Thread the `auto_approve` CLI flag into the chat session approval behavior instead of dropping it on the floor.
- Preserve the existing permission model in `quine-core`; the intended fix is to make the CLI automatically answer approval requests with approval when the user explicitly opts in.
- Scope the change narrowly to session behavior used by chat flows so that all operations that surface as approval requests receive a consistent automatic approval response.
- Confirm whether there are multiple chat entry points or daemon/client interaction paths that can emit approval requests and ensure the same auto-approve behavior is applied to each relevant path.
- Keep trait and cross-crate interfaces stable unless research during implementation shows a smaller change is impossible.

## File-by-File Changes

- `crates/quine-cli/src/chat.rs`
  - Replace the current no-op handling of `auto_approve` with real approval request handling in the chat client event loop.
  - Auto-submit approval responses for every incoming approval request event when the flag is enabled, so the behavior applies uniformly across approval-gated operations surfaced by chat.
  - Reuse the existing client/session APIs instead of introducing new approval semantics in core.
- `crates/quine-cli/src/run.rs`
  - Verify whether the non-interactive/resume flow can also receive approval requests and, if so, apply the same automatic approval behavior there so `--auto-approve` truly covers all operation approvals exposed by CLI chat/run flows.
- `crates/quine-cli/src/main.rs`
  - Keep the existing flag plumbing, but update command help text so `--auto-approve` no longer claims it has no effect.
- `crates/quine-cli` tests
  - Add or update focused tests around approval request handling in `chat` and any shared/non-interactive loop used by `run`.
- `crates/quine-core/src/permission/approval.rs`
  - No interface change planned; implementation should only confirm the exact approval request/response contract the CLI must answer.

## Validation Plan

- Run targeted unit tests for the CLI approval-handling flow, starting with the smallest affected tests in `quine-cli`.
- Run an end-to-end daemon-backed command that exercises an approval-gated operation with `--auto-approve` enabled and verify it completes without prompting interactively.
- Run the same scenario without `--auto-approve` and verify the approval request remains gated.
- Run `cargo test -p quine-cli` at minimum, and expand as needed if shared code paths are touched.
- Run `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all -- --check` before handoff.

## QA Feedback

- The QA plan should explicitly use the existing daemon startup flow exposed by `cargo run --bin quine-harness -- start --socket <path>` and the client commands `cargo run --bin quine -- chat --socket <path> --auto-approve` or `cargo run --bin quine -- run --socket <path> --auto-approve "<message>"`, depending on which path the implementation touches.
- The QA plan should verify the command help text regression too, since today `quine chat --help` says `--auto-approve` is parsed but has no effect.
- A deterministic approval-gated scenario can use a write-capable tool or any existing fixture/test harness operation already known to emit an approval request; implementation should document the exact prompt once selected.
