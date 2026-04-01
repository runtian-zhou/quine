# 037 Persistent Memory — Implementation Plan

Short summary: Add a trait-based persistent memory subsystem with `quine-core` domain types and prompt rendering, a filesystem-backed `quine-harness` store, and CLI `/memory` commands for explicit memory management.

## Open Questions

- None. The feature scope is intentionally limited to explicit memory CRUD plus prompt injection at session startup.

## Agreement Status

agreed — the implementation scope, storage model, CLI surface, and QA expectations are aligned, and there are no unresolved planning questions.

## Proposed Design

- Keep the interface contract in `quine-core`:
  - `MemoryScope` for `user`, `project`, and `session`
  - `MemoryRecord` and versioned `MemoryDocument`
  - `MemoryService` trait for listing, loading applicable records, upserting, and deleting
  - deterministic memory rendering helper used during session initialization
- Load memory into the effective system prompt during `SessionContext::new`.
- Keep memory explicitly separate from transcript history, compaction, and skills.
- Implement a harness-owned `FilesystemMemoryStore`:
  - store files under `<state_dir>/memory/`
  - use `user.json`, `projects/<project-key>.json`, and `sessions/<session-id>.json`
  - canonicalize project roots where possible for stable lookup
  - sort records deterministically before returning them
  - use temporary-file replacement for writes
- Thread the memory service into `LocalHarness` startup and from there into `quine-core` session creation.
- Extend the harness service and JSON-RPC protocol with:
  - `list_memory`
  - `upsert_memory`
  - `delete_memory`
- Add interactive CLI support for:
  - `/memory list <scope>`
  - `/memory add <scope> <title>: <body>`
  - `/memory delete <scope> <id>`
- Keep memory writes user-driven in v1; do not add a model-facing write tool.

## File-by-File Changes

- `crates/quine-core/src/memory.rs`
  - Add core memory domain types, trait, rendering helper, and unit tests.
- `crates/quine-core/src/engine.rs`
  - Load applicable memory through `MemoryService` during session initialization and append a rendered memory section to the system prompt.
- `crates/quine-core/src/lib.rs`
  - Re-export memory types and helpers.
- `crates/quine-harness/src/memory_store.rs`
  - Implement the durable filesystem-backed memory service.
- `crates/quine-harness/src/local.rs`
  - Instantiate the memory store and pass it into the core startup path.
- `crates/quine-harness/src/service.rs`
  - Extend the harness trait with memory CRUD methods.
- `crates/quine-harness/src/protocol.rs`
  - Add JSON-RPC method constants for memory operations.
- `crates/quine-harness/src/server.rs`
  - Parse memory RPC requests and forward them through the harness service.
- `crates/quine-harness/src/config.rs`
  - Document or expose the memory directory helper rooted in the harness state directory.
- `crates/quine-cli/src/slash_command.rs`
  - Parse `/memory` and its subcommands.
- `crates/quine-cli/src/chat.rs`
  - Execute `/memory` list/add/delete flows through the daemon RPC client.
- `features/037-persistent-memory.md`
  - Record the feature contract and acceptance criteria.
- `features/plans/037-persistent-memory-qa.md`
  - Capture the QA strategy for the feature.
- `CLAUDE.md`
  - Document the memory subsystem and storage conventions.

## Validation Plan

- Run targeted tests first:
  - `cargo test -p quine-core memory -- --nocapture`
  - `cargo test -p quine-harness memory_store -- --nocapture`
  - `cargo test -p quine-harness local:: -- --nocapture`
  - `cargo test -p quine-cli slash_command -- --nocapture`
  - `cargo test -p quine-cli chat:: -- --nocapture`
- Run broader quality gates before handoff:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- QA agrees with keeping writes explicit in v1 and treating prompt injection plus CLI-driven CRUD as the correct first slice.
- QA expects deterministic ordering of rendered memory and file-backed records so snapshots and user experience stay stable.
- QA will verify that memory remains separate from transcript compaction and that no model-initiated write path is introduced in this feature.
- QA will require project-scope handling to be stable enough that the same working directory resolves to the same stored memory document across sessions.
