---
status: done
---

# Prompt-Time Persistent Memory Injection and Recall

## Overview

Implement the fourth Quine memory-system slice by surfacing durable memory during prompt construction.

This feature should support both baseline `MEMORY.md` index injection and targeted relevant-memory recall, while keeping prompt-construction behavior bounded, deterministic, and explainable.

The goal is to make persistent memory useful during live conversations after the durable store and extraction pipeline already exist.

## Requirements

### 1. Add baseline persistent-memory injection

Extend prompt construction so Quine can inject a bounded baseline durable-memory context, such as `MEMORY.md`, when the feature is enabled.

The implementation should ensure:

- prompt ordering is deterministic
- injected memory volume is bounded
- stale-memory caveats remain explicit

### 2. Add targeted relevant-memory recall

Add a bounded targeted-recall path that selects only the most relevant durable-memory entries for a given turn.

The initial selection strategy should be deterministic and should rely on simple heuristics such as:

- latest user-message overlap
- recency
- pinning or metadata hints
- scope match

### 3. Keep prompt injection internal and additive

Prompt-construction changes should stay internal to `quine-core` where possible.

If any diagnostics or inspection surfaces are added, they should be additive and should explain which injection mode ran and which memory entries were selected.

### 4. Preserve disabled-mode behavior

When persistent memory injection is disabled, prompt construction should remain behaviorally unchanged.

### 5. Add focused tests

Add tests for:

- baseline index injection ordering
- relevant-memory selection and ranking
- truncation and budget handling
- disabled-mode no-op behavior
- prompt-construction integration when recall is enabled

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Quine can inject bounded baseline persistent-memory context when enabled.
- Quine can select and inject a bounded set of relevant durable memories deterministically.
- Prompt construction remains unchanged when the feature is disabled.
- No shared inter-crate trait contract is modified to make this feature work.

## Non-Goals (Deferred)

- Team-scoped or agent-scoped advanced memory policy
- Rich UI for memory curation
- LLM-based relevance selection in the first release if deterministic heuristics are sufficient
