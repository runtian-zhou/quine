---
status: in-progress
---

# Persistent Memory

## Overview

Add a durable memory subsystem that lets Quine carry forward explicit user, project, and session facts across runs without conflating that information with transcript history or compaction archives.

The first version should support explicit memory management through the CLI and harness RPC surface. Memory is loaded into the initial effective system context for each session, but it is not extracted automatically from arbitrary model output.

## Requirements

### 1. Define core-owned memory abstractions

Add a `quine-core` memory domain that includes:

- durable memory record types
- scope definitions for user, project, and session memory
- a trait-based `MemoryService` boundary owned by `quine-core`
- a deterministic prompt-rendering helper for injecting memory into session context

This must follow the workspace rule that crate boundaries are expressed through traits, not concrete types.

### 2. Keep memory separate from transcript history

Memory must remain distinct from:

- turn-by-turn conversation history
- archived compacted transcripts
- skills loaded from `.quine/skills/` or `.claude/commands/`

Compaction should not silently rewrite or synthesize durable memory records in the first version.

### 3. Load applicable memory at session startup

When a session starts, Quine should resolve and load applicable memory from:

- user scope
- project scope for the working directory
- session scope for that session identifier

Applicable memory should be rendered into a dedicated memory section inside the initial system prompt context.

### 4. Add a durable local harness backend

Add a filesystem-backed memory store in `quine-harness` that persists memory under the harness state directory.

The first version must persist JSON documents for:

- user memory
- project memory
- session memory

The implementation should:

- use a schema version field
- tolerate missing files by treating them as empty
- use deterministic project keys
- use safe write/replace behavior rather than partial in-place writes

### 5. Expose memory operations over harness RPC

Extend the harness service and JSON-RPC protocol with explicit methods to:

- list memory by scope
- create or update a memory record
- delete a memory record

The RPC surface should stay explicit and not depend on model-generated tool calls for writes.

### 6. Add CLI memory commands

Add a `/memory` slash-command flow in the interactive CLI that supports at least:

- `/memory list <user|project|session>`
- `/memory add <user|project|session> <title>: <body>`
- `/memory delete <user|project|session> <id>`

The CLI output should stay terse and consistent with the existing renderer style.

### 7. Document the subsystem clearly

Update `CLAUDE.md` so future agents can discover and correctly extend the memory subsystem, including:

- the role of memory in the architecture
- the separation between memory and transcript history
- the storage conventions under harness state

Add implementation and QA planning docs under `features/plans/` for this feature.

### 8. Add focused tests

Add tests for:

- memory serialization and prompt rendering
- harness filesystem CRUD behavior
- scope-specific loading of applicable memory
- CLI parsing for `/memory` commands

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Sessions load applicable user/project/session memory into the initial system context.
- Memory records persist under the harness state directory across runs.
- Users can list, add, and delete memory via the interactive CLI.
- Memory remains separate from transcript history and compaction archives.

## Non-Goals (Deferred)

- Automatic extraction of memory from arbitrary model output
- Semantic memory search or retrieval ranking
- Rich interactive memory editing UI
- Model-initiated memory writes without explicit user mediation
- Synchronizing memory across multiple machines or remote daemons
