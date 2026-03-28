---
status: done
---

# Spawn and Subagent QA Smoke Coverage

## Overview

Add a small, focused QA feature that makes it easier to verify the current implementations of `spawn` and `subagent` behave as intended.

The goal is not to redesign child-agent orchestration. Instead, this feature adds narrow validation coverage for the existing contract:

- `subagent` is the simple synchronous delegation tool that returns a final result directly.
- `spawn` is the lower-level asynchronous primitive that returns a child session identifier for later coordination.

## Requirements

### 1. Add focused automated coverage

Add or extend tests that validate these behaviors in the current implementation:

- `spawn` returns a child session identifier when a core input channel is available.
- `spawn` reports a clear error when no core input channel is available.
- `subagent` remains usable for direct delegation and returns a final text result.
- `subagent` timeout behavior remains covered.

### 2. Add lightweight QA guidance

Add a small QA artifact or test case documentation that explains how to manually confirm the distinction between `spawn` and `subagent` in a realistic workflow.

The guidance should make it easy to verify:

- `subagent` completes the delegated task inline.
- `spawn` creates a child session that can be observed or awaited separately.

### 3. Preserve current architecture

Do not change crate-boundary traits or redesign session orchestration. Keep the change limited to tests, QA guidance, and any tiny supporting refactors needed to make those checks straightforward.

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- There is explicit automated coverage for the success and failure paths of `spawn`.
- There is explicit automated or documented QA coverage that distinguishes `spawn` from `subagent` behavior.
