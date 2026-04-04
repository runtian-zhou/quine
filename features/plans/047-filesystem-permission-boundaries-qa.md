# 047 Filesystem Permission Boundaries — QA Plan

Short summary: Verify Quine Feature 5 shared filesystem permission boundaries, including canonical path checks, workspace/additional-directory authorization, read/write distinction, and negative coverage for outside-root access.

## Open Questions

- None. This plan stays scoped to Feature 5 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest `features/plans/047-filesystem-permission-boundaries-implementation.md` revision and confirmed the paired docs now match on shared path authorization design, exact test selectors, and the concrete daemon-backed scenario. Both docs are aligned and have no unresolved open questions.

## Test Strategy

- Validate the feature at three layers:
  - focused `quine-core` unit tests for canonicalization, containment, traversal, additional-root, and symlink policy helpers
  - `quine-core` integration tests that prove `read_file`, `find`, and `apply_patch` all consume the same shared authorization logic
  - at least one real local-daemon multi-round chat scenario because this feature changes `quine-core` runtime behavior
- Keep pass criteria limited to Feature 5 behavior only:
  - workspace root containment
  - additional approved roots
  - final resolved-path evaluation for traversal and symlink cases
  - deterministic outside-root denials
  - read versus write boundary differences only where this slice explicitly introduces them
- Prefer exact test selectors and concrete daemon commands so another agent can execute QA without inventing missing details.
- Prefer daemon evidence through one-off CLI flows already used in prior plans: `cargo run --bin quine -- daemon start`, `run --json`, and `respond --json` where multi-round behavior is needed.

## Scenarios

- **Unit — Workspace Root Containment**
  - Add a focused `quine-core` unit test named `permission::path::tests::workspace_root_allows_resolved_in_bounds_paths` or an equivalently precise selector.
  - Run `cargo test -p quine-core workspace_root_allows_resolved_in_bounds_paths -- --exact --nocapture`.
  - Fixture/setup:
    - create a temporary session filesystem rooted at a temp workspace directory
    - evaluate a candidate path such as `./src/../Cargo.toml` that resolves inside the workspace root
  - Expected result:
    - the helper resolves the final path before authorization
    - the resolved in-workspace path is allowed for the intended scope under test
    - the assertion proves the policy is based on containment of the resolved target, not the raw lexical string

- **Unit — Additional Approved Root Containment**
  - Add a focused `quine-core` unit test named `permission::path::tests::additional_root_allows_resolved_in_bounds_paths`.
  - Run `cargo test -p quine-core additional_root_allows_resolved_in_bounds_paths -- --exact --nocapture`.
  - Fixture/setup:
    - create a temp workspace root and a second temp directory outside that workspace
    - register the second directory as an additional allowed root in `PermissionContext`
    - evaluate a target such as `<additional-root>/notes/allowed.txt`
  - Expected result:
    - the resolved target is treated as in-bounds even though it is outside the workspace root
    - the allow result is identical to the in-workspace case for the same access scope
    - no unrelated root is implicitly allowed

- **Unit — Outside-All-Roots Denial**
  - Add a focused `quine-core` unit test named `permission::path::tests::outside_all_roots_is_denied`.
  - Run `cargo test -p quine-core outside_all_roots_is_denied -- --exact --nocapture`.
  - Fixture/setup:
    - create a temp workspace root and one additional allowed root
    - evaluate a third temp directory that is outside both approved roots
  - Expected result:
    - the helper returns a deterministic deny result for the outside target
    - the deny result is the same regardless of whether the raw path is absolute or relative after resolution against the session working directory

- **Unit — Traversal Resolves to Final Target**
  - Add a focused `quine-core` unit test named `permission::path::tests::traversal_is_evaluated_on_final_resolved_target`.
  - Run `cargo test -p quine-core traversal_is_evaluated_on_final_resolved_target -- --exact --nocapture`.
  - Fixture/setup:
    - evaluate one path containing traversal segments that still resolves inside the workspace, such as `fixtures/../allowed.txt`
    - evaluate a second path containing traversal segments that resolves outside all approved roots
  - Expected result:
    - the in-bounds traversal case is allowed
    - the out-of-bounds traversal case is denied
    - assertions explicitly prove the implementation does not deny or allow purely on the presence of `..`

- **Unit — Symlink Escape Denied**
  - Add a focused `quine-core` unit test named `permission::path::tests::symlink_escape_is_denied_if_supported`.
  - Run `cargo test -p quine-core symlink_escape_is_denied_if_supported -- --exact --nocapture`.
  - Fixture/setup:
    - inside the temp workspace, create a symlink such as `workspace/link-out` that points to a file outside every approved root
    - skip this test only on platforms where the test runner cannot create the symlink without special privileges; the skip reason must be explicit in test output
  - Expected result:
    - authorization follows the final resolved target, not the symlink’s lexical location inside the workspace
    - the escaped target is denied deterministically

- **Integration — Shared Helper Governs `read_file`, `find`, and `apply_patch`**
  - Add a `quine-core` integration test file, for example `crates/quine-core/tests/filesystem_permission_boundaries.rs`, with a deterministic permission context and session filesystem fixture.
  - Run `cargo test -p quine-core --test filesystem_permission_boundaries -- --nocapture`.
  - Fixture/setup:
    - workspace root containing `allowed/readable.txt`
    - additional approved root containing `external/allowed.txt`
    - outside root containing `outside/forbidden.txt`
    - one file target for write attempts such as `outside/forbidden-write.txt`
  - Expected result for tool-level cases in the same integration suite:
    - `read_file` on `allowed/readable.txt` succeeds with normal tool output
    - `read_file` on `<additional-root>/external/allowed.txt` succeeds if the implementation exposes additional-root access in this slice
    - `read_file` on `<outside-root>/outside/forbidden.txt` fails with the deterministic permission-denied outcome chosen by implementation
    - `find` rooted at `.` succeeds and returns in-workspace matches only
    - `find` rooted at `<additional-root>` succeeds if that root is configured as approved
    - `find` rooted at `<outside-root>` fails with deterministic denial rather than traversing the outside tree
    - `apply_patch` writing a workspace file succeeds when the target resolves in-bounds
    - `apply_patch` writing `<outside-root>/outside/forbidden-write.txt` fails closed and leaves the file absent or unchanged
  - This integration file must assert that the deny contract is observably consistent across all three tools rather than each tool reimplementing different containment logic.

- **Daemon Multi-Round — Outside-Root Write Request Is Rejected**
  - Use a real local daemon because this feature affects `quine-core` runtime behavior.
  - Start the daemon in one terminal: `source .env && cargo run --bin quine -- daemon start --socket /tmp/quine-047.sock`.
  - In a second terminal, send round 1 with: `cargo run --bin quine -- run --json --socket /tmp/quine-047.sock "Use apply_patch to create ../qa-outside-root-denied.txt containing exactly OUTSIDE_ROOT_DENIED_047."`
  - Round 1 user message: `Use apply_patch to create ../qa-outside-root-denied.txt containing exactly OUTSIDE_ROOT_DENIED_047.`
  - Expected round 1 result:
    - the command returns structured JSON for the one-shot session
    - `tool_calls` shows an attempted `apply_patch` invocation targeting `../qa-outside-root-denied.txt` or its resolved equivalent
    - final assistant text explicitly reports that the write could not be completed because the target is outside the allowed workspace/additional roots, or the tool result surfaces the implementation’s deterministic permission-denied error text
    - there is no success text claiming the file was created
    - no approval prompt or unrelated interaction flow appears, because this feature is about boundary enforcement rather than operator approval routing
  - Round 2 user message in the same session: `Now use read_file on Cargo.toml and tell me the workspace package names you see.`
  - Run round 2 with: `cargo run --bin quine -- run --json --socket /tmp/quine-047.sock --session <SESSION_ID> "Now use read_file on Cargo.toml and tell me the workspace package names you see."`
  - Expected round 2 result:
    - `tool_calls` shows `read_file`
    - the command completes without an approval prompt
    - final assistant text contains at least one real workspace package/crate name from `Cargo.toml` or clearly reports the workspace members read from that file
    - this proves a denied outside-root write does not wedge the session and does not block subsequent in-bounds reads
  - Cleanup after the scenario:
    - verify `../qa-outside-root-denied.txt` does not exist relative to the workspace root used for the daemon session
    - stop the daemon with `cargo run --bin quine -- daemon stop --socket /tmp/quine-047.sock`

- **Daemon One-Off — In-Workspace Search Still Succeeds**
  - Start the daemon: `source .env && cargo run --bin quine -- daemon start --socket /tmp/quine-047.sock`.
  - Run: `cargo run --bin quine -- run --json --socket /tmp/quine-047.sock "Use find with path '.' pattern 'Cargo.toml' and type 'file'. Report the exact relative path you found."`
  - Expected result:
    - `tool_calls` shows `find`
    - final assistant text reports `Cargo.toml`
    - no denial or approval prompt appears
    - this confirms normal in-bounds reads/searches still work after the shared helper is introduced
  - Cleanup:
    - stop the daemon with `cargo run --bin quine -- daemon stop --socket /tmp/quine-047.sock`

## Required Evidence

- Passing focused test results, captured as exact command lines and pass/fail output, for:
  - `cargo test -p quine-core workspace_root_allows_resolved_in_bounds_paths -- --exact --nocapture`
  - `cargo test -p quine-core additional_root_allows_resolved_in_bounds_paths -- --exact --nocapture`
  - `cargo test -p quine-core outside_all_roots_is_denied -- --exact --nocapture`
  - `cargo test -p quine-core traversal_is_evaluated_on_final_resolved_target -- --exact --nocapture`
  - `cargo test -p quine-core symlink_escape_is_denied_if_supported -- --exact --nocapture`
  - `cargo test -p quine-core --test filesystem_permission_boundaries -- --nocapture`
- One daemon-backed multi-round transcript or captured JSON output showing:
  - `source .env && cargo run --bin quine -- daemon start --socket /tmp/quine-047.sock`
  - the exact round 1 `run --json` invocation for the outside-root `apply_patch` attempt
  - the returned `session_id`
  - the exact round 2 `run --json --session <SESSION_ID>` invocation for the in-bounds `read_file` follow-up
  - final JSON output proving the write was denied but the session continued and the read succeeded
- One one-off daemon transcript or captured JSON output showing the exact `find` invocation and successful in-bounds result.
- Explicit filesystem evidence for the negative write case:
  - `../qa-outside-root-denied.txt` was not created, or any preexisting target remained unchanged
- If the implementation lands additional-root runtime wiring in this slice, evidence that both unit and integration coverage exercised an approved additional root successfully; if runtime wiring does not land yet, QA may satisfy additional-root coverage through unit/integration tests only and should record that the daemon flow stayed scoped to workspace-root behavior.
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Re-reviewed `features/plans/047-filesystem-permission-boundaries-implementation.md` before updating this QA plan.
- The implementation plan’s scope, file targeting, and validation categories align with this QA plan: shared path-policy logic in `quine-core`, reuse by `read_file`, `find`, and `apply_patch`, additive additional-root modeling, and deterministic denial for outside-root writes.
- This QA revision addresses the implementation doc’s prior feedback by adding:
  - exact focused test commands and expected assertions
  - exact daemon start/connect commands
  - one concrete multi-round local-daemon scenario with round-by-round messages and expected outcomes
  - explicit per-tool allow/deny expectations and explicit negative write evidence
- No new implementation-scope changes are requested from QA at this revision.
