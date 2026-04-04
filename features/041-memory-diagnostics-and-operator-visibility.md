---
status: done
---

# Memory Diagnostics and Operator Visibility

## Overview

Implement the fifth Quine memory-system slice by adding structured diagnostics and operator visibility for session memory and persistent memory behavior.

This feature should expose enough read-only information for debugging, QA, and CLI inspection without adding mutation or repair flows.

The goal is to make memory behavior observable and testable before advanced scopes and broader rollout.

## Requirements

### 1. Add structured per-turn memory diagnostics

Introduce structured diagnostics that can explain:

- whether session memory refreshed
- which summary path and boundary were used
- whether persistent-memory injection ran
- which durable-memory entries were selected or skipped
- why truncation, fallback, or stale-memory decisions occurred

### 2. Expose diagnostics through additive inspection surfaces

Diagnostics should be exposed through additive harness/session inspection surfaces and any existing CLI debug/context views.

The surface should remain read-only and observational.

### 3. Keep diagnostics bounded and structured

The diagnostics payload should be structured enough for automated QA and should avoid relying on unstructured debug logs as the primary contract.

### 4. Preserve existing runtime behavior

This feature should not change memory semantics. It should improve visibility only.

### 5. Add focused tests

Add tests for:

- diagnostics payload serialization
- additive session snapshot exposure
- end-to-end inspection of a turn that updates or injects memory
- CLI or renderer behavior where applicable

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Quine exposes structured read-only diagnostics for session memory and persistent memory behavior.
- Diagnostics are accessible through additive inspection surfaces without mutating memory state.
- The diagnostics payload is sufficient for QA to validate why memory was used, skipped, or updated.
- No shared inter-crate trait contract is modified to make this feature work.

## Non-Goals (Deferred)

- Diagnostic-triggered repair or retry operations
- Rich memory-management UI beyond inspection/debug visibility
- Broad public memory-management APIs
