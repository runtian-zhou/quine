# 048 Bash Command Risk Policy — QA Plan

Short summary: Verify Quine Feature 6 command-aware `bash` risk policy, including deterministic shell classification, regression coverage for dangerous patterns, and differentiated policy outcomes for safer versus higher-risk commands.

## Open Questions

- None. This plan stays scoped to Feature 6 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest `features/plans/048-bash-command-risk-policy-implementation.md` revision and confirmed the paired docs now match on deterministic command classification, exact test targets, representative commands, and expected evaluator outcomes. Both docs are aligned and have no unresolved open questions.

## Test Strategy

- Start with deterministic table-driven `quine-core` tests for shell classification.
- Add integration coverage that exercises the evaluator with richer command metadata.
- Keep this slice focused on `bash`; do not extend QA scope to unrelated tools.

## Scenarios

- **Unit — Read-Oriented Command Classification**
  - **Target**: `cargo test -p quine-core bash::tests -- --nocapture`
  - **Expected coverage location**: new or expanded table-driven unit tests in `crates/quine-core/src/permission/command.rs` and, if needed for request construction, adjacent assertions in `crates/quine-core/src/tool/bash.rs`.
  - **Representative commands**: `pwd`, `ls`, `ls -la`, `find . -maxdepth 1 -type f`.
  - **Expected result**: each command produces read-oriented / low-risk command metadata, including normalized command text or argv-derived metadata that distinguishes these commands from generic execute-only requests.
- **Unit — Write-Capable Command Classification**
  - **Target**: `cargo test -p quine-core bash::tests -- --nocapture`
  - **Expected coverage location**: table-driven classification tests in `crates/quine-core/src/permission/command.rs`.
  - **Representative commands**: `echo hello > test.txt`, `touch test.txt`, `mkdir scratch`, `rm -f test.txt`.
  - **Expected result**: each command is classified into a mutating / higher-risk bucket rather than the read-oriented bucket, and the resulting request metadata preserves enough detail for the evaluator to distinguish these commands from `pwd` or `ls`.
- **Unit — Dangerous Nested Shell Classification**
  - **Target**: `cargo test -p quine-core bash::tests -- --nocapture`
  - **Expected coverage location**: regression cases in `crates/quine-core/src/permission/command.rs`.
  - **Representative commands**: `sh -c 'pwd'`, `bash -lc 'ls -la'`, `env sh -c 'echo hello'`.
  - **Expected result**: each command is classified into the high-risk nested-shell bucket even when the inner command text looks read-only, and no nested-shell wrapper is downgraded to the read-oriented classification.
- **Unit — Interpreter Launcher Classification**
  - **Target**: `cargo test -p quine-core bash::tests -- --nocapture`
  - **Expected coverage location**: regression cases in `crates/quine-core/src/permission/command.rs`.
  - **Representative commands**: `python -c 'print(1)'`, `python -c 'from pathlib import Path; Path("x").write_text("y")'`, `perl -e 'print qq(hi)'`.
  - **Expected result**: each launcher is classified conservatively into a high-risk interpreter bucket regardless of whether the inline snippet appears read-only, preventing broad interpreter escapes from inheriting low-risk treatment.
- **Integration — Evaluator Differentiates Safer vs Higher-Risk Bash Requests**
  - **Target**: `cargo test -p quine-core permission -- --nocapture`
  - **Expected coverage location**: evaluator-focused tests in `crates/quine-core/src/permission/engine.rs` and/or `crates/quine-core/src/permission/request.rs` that construct `bash` permission requests carrying command-risk metadata.
  - **Setup**: create one policy configuration that allows read-oriented `bash` requests to proceed without escalation while treating mutating or high-risk requests as denied or escalation-required.
  - **Representative requests**: one request built from `pwd`, one from `echo hello > test.txt`, and one from `sh -c 'pwd'` or `python -c 'print(1)'`.
  - **Expected result**: the `pwd` request is evaluated as the lower-risk case under that shared policy, while the write-capable and nested-shell/interpreter requests produce a different outcome because their command-risk metadata differs; any denial or escalation reason mentions the command-risk distinction rather than a generic undifferentiated execute permission.

## Required Evidence

- Passing table-driven shell classification tests.
- Passing regression tests for nested shell and interpreter patterns.
- Passing integration evidence that richer request metadata changes policy outcomes appropriately.
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Reviewed the latest `features/plans/048-bash-command-risk-policy-implementation.md`; the implementation scope remains appropriately limited to deterministic `bash` command-risk analysis plus evaluator metadata plumbing.
- This QA plan now matches that scope with concrete unit and integration targets, representative command strings, and expected classification or policy outcomes.
- No additional implementation-plan changes are required from QA at this revision.
