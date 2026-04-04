---
status: done
---

# Session-Memory-Driven Compaction

## Overview

Implement the second Quine memory-system slice by integrating session memory into compaction.

This feature should make compaction prefer the structured `session-memory/summary.md` plus its boundary metadata when that state is valid, while preserving the current generic compaction summarizer path as a safe fallback.

The goal is to improve long-session continuity during transcript compaction without yet introducing persistent cross-session memory or prompt-time durable-memory injection.

## Requirements

### 1. Consume session memory during compaction when available

Extend `quine-core` compaction so it can use the maintained session-memory summary and boundary marker as its preferred compact summary input.

The implementation should:

- load the current session-memory summary state
- validate the summary and last summarized boundary
- preserve the live unsummarized tail after the recorded boundary
- rebuild compacted history around the session-memory summary rather than the generic summarizer when safe

### 2. Preserve the existing fallback compaction path

If session-memory state is missing, invalid, stale, or unusable, Quine must retain the current compaction flow.

This feature must not make compaction less reliable. Session-memory-driven compaction is an optimization and quality improvement, not a hard dependency.

### 3. Coordinate compaction with in-flight summary updates

Compaction must consume a consistent session-memory snapshot.

The implementation should define how compaction behaves when a session-memory refresh is already in flight, including any waiting, retry, or fallback behavior needed to avoid inconsistent boundaries.

### 4. Preserve existing compaction invariants

The feature must preserve existing behavior around:

- transcript archive generation
- system-message preservation
- live-tail preservation
- archived tool-result handling
- safe compaction fallback

### 5. Keep the change internal to core

This feature should primarily modify internal `quine-core` compaction behavior.

It should not require a new broad external API surface. Any diagnostics exposure should remain additive and optional.

### 6. Add focused tests

Add tests for:

- valid session-memory compaction selection
- invalid/missing session-memory fallback to legacy compaction
- boundary selection correctness
- live-tail preservation
- coordination with in-flight session-memory refresh state
- regression coverage for existing compaction/archive behavior

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- When valid session-memory summary data exists, compaction uses it as the compact summary source.
- When session-memory state is missing or invalid, compaction falls back to the existing legacy compaction path.
- Existing tool-result archiving and live-tail preservation behavior remains intact.
- No shared inter-crate trait contract is modified to make this feature work.

## Non-Goals (Deferred)

- Creating session memory itself
- Persistent cross-session memory extraction or recall
- Prompt-time `MEMORY.md` injection or targeted durable-memory recall
- Team-scoped or agent-scoped memory resolution
