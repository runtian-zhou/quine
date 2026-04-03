---
status: pending
---

# Persistent Memory Store and Durable Extraction

## Overview

Implement the third Quine memory-system slice by introducing a project-scoped persistent memory store and a conservative durable-memory extraction pipeline.

This feature should create durable, inspectable memory files under Quine-managed state, maintain a `MEMORY.md` index plus one-memory-per-file entries, and update those entries from explicit or conservative post-turn extraction decisions.

The goal is to establish persistent cross-session memory storage and maintenance before adding prompt-time injection or targeted recall.

## Requirements

### 1. Add a project-scoped durable memory store

Define a Quine-owned, project-scoped durable-memory layout under harness-managed state.

The first version should support:

- a stable root for project-scoped memory
- an index file such as `MEMORY.md`
- one durable memory per markdown file
- structured metadata/frontmatter for each entry

### 2. Support conservative post-turn memory extraction

Add a post-turn extraction pipeline that can create, update, tombstone, or ignore persistent memories.

The first release should be conservative and should support at least:

- explicit user intent like “remember this” or “forget this”
- limited heuristic extraction of durable facts
- deterministic index maintenance after memory updates

### 3. Keep durable memory human-readable and inspectable

Persistent memory files and indexes must remain easy to inspect and curate manually on disk.

The feature should clearly separate durable memory content from transient session-memory artifacts.

### 4. Preserve prompt behavior for now

This feature should not yet inject persistent memory into prompt construction except for any minimal internal validation that does not change user-visible behavior.

Prompt-time baseline injection and targeted recall are deferred to the next feature slice.

### 5. Add focused config and storage behavior

Any config or path overrides required for persistent memory should be additive and scoped to the durable-memory store only.

The implementation should make clear how the persistent memory root is resolved from project state, trusted overrides, and testing/development overrides.

### 6. Add focused tests

Add tests for:

- path resolution
- frontmatter parsing and rendering
- one-memory-per-file persistence behavior
- `MEMORY.md` index generation and maintenance
- explicit remember/forget flows
- durable-memory persistence across harness restarts and new sessions in the same project

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Quine can create and maintain a project-scoped durable memory store under Quine-managed state.
- Durable-memory extraction can create, update, tombstone, or ignore memory entries conservatively.
- `MEMORY.md` remains an inspectable index and memory payloads remain in individual files.
- Durable-memory data persists across harness restarts and across new sessions in the same project.
- No shared inter-crate trait contract is modified to make this feature work.

## Non-Goals (Deferred)

- Prompt-time `MEMORY.md` injection
- Targeted relevant-memory recall
- Team-scoped or agent-scoped durable memory variants
- Rich user-facing memory editing UI
