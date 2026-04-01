# 037 Persistent Memory — QA Plan

Short summary: Verify that Quine persists explicit user/project/session memory, injects applicable memory into session startup context, and exposes stable CLI and harness flows for listing and mutating memory.

## Open Questions

- None. The feature scope is clear: explicit persistent memory with CLI-managed CRUD and session-start prompt injection.

## Agreement Status

agreed — reviewed the implementation plan and current scoped behavior; the storage, startup injection, RPC, and CLI expectations are aligned.

## Test Strategy

- Validate the feature at three layers:
  - `quine-core` unit coverage for memory modeling and prompt rendering
  - `quine-harness` tests for filesystem persistence and applicable-scope loading
  - `quine-cli` parsing and command-path coverage for `/memory`
- Keep QA focused on explicit user-controlled memory rather than autonomous extraction or retrieval heuristics.
- Treat documentation review as part of acceptance because future agents must be able to discover the subsystem from `CLAUDE.md` and feature docs.

## Scenarios

- **Scenario 1: Core memory serialization and rendering**
  - Command: `cargo test -p quine-core memory -- --nocapture`
  - Exact validation: memory records serialize and deserialize correctly, empty memory renders no section, and non-empty memory renders deterministic prompt text.
  - Expected result: core-owned memory abstractions are stable and prompt-ready.

- **Scenario 2: Harness filesystem CRUD behavior**
  - Command: `cargo test -p quine-harness memory_store -- --nocapture`
  - Exact validation: missing files behave as empty memory, upsert replaces by record ID, and applicable memory combines user/project/session scopes.
  - Expected result: the filesystem backend persists and resolves memory deterministically.

- **Scenario 3: Harness session startup remains healthy with memory enabled**
  - Command: `cargo test -p quine-harness local:: -- --nocapture`
  - Exact validation: normal local harness session flows still succeed after memory store wiring is introduced.
  - Expected result: memory startup integration does not regress regular session creation or message flow.

- **Scenario 4: CLI slash parsing for memory commands**
  - Command: `cargo test -p quine-cli slash_command -- --nocapture`
  - Exact validation: `/memory list`, `/memory add`, and `/memory delete` parse correctly, and invalid scopes are rejected with a clear local error.
  - Expected result: the CLI command grammar is stable and explicit.

- **Scenario 5: CLI chat command dispatch remains healthy**
  - Command: `cargo test -p quine-cli chat:: -- --nocapture`
  - Exact validation: existing chat command flows still behave correctly with `/memory` support added to the command router.
  - Expected result: command dispatch remains backwards-compatible for chat mode.

- **Scenario 6: Workspace quality gates**
  - Commands:
    - `cargo build`
    - `cargo test`
    - `cargo clippy --all-targets -- -D warnings`
    - `cargo fmt --all -- --check`
  - Exact validation: the full workspace builds cleanly, tests pass, formatting is correct, and no warnings remain.
  - Expected result: the memory feature integrates cleanly with the existing codebase.

## Required Evidence

- Passing targeted test output for `quine-core`, `quine-harness`, and `quine-cli` memory-related coverage.
- Evidence that harness-backed session creation still works after memory wiring.
- Repository inspection evidence that:
  - memory is documented separately from transcript history and compaction
  - `CLAUDE.md` describes the new subsystem and storage conventions clearly
  - the feature request and plan docs exist for this feature
- Passing workspace quality gates before feature handoff.

## Implementation Feedback

- QA agrees with the decision to keep memory writes explicit and CLI-driven in v1.
- QA will inspect project-scope keying and deterministic ordering carefully because both affect long-term usability and reproducibility.
- QA expects the documentation to clearly distinguish durable memory from conversation history so future work does not blur those systems.
