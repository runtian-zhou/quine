---
status: done
---

# Advanced Memory Scopes and Policy Controls

## Overview

Implement the sixth Quine memory-system slice by adding advanced durable-memory scopes and policy controls.

This feature should extend durable memory beyond the default project scope to optional team and agent scopes, and should define deterministic read/write policy, conflict resolution, and feature-gating behavior.

The goal is to support richer memory organization only after the core project-scoped store, prompt-time recall, and diagnostics are already stable.

## Requirements

### 1. Add advanced durable-memory scopes

Extend durable-memory resolution to support:

- project scope as the default
- optional agent scope
- optional team scope

The implementation should keep scope resolution deterministic and easy to reason about.

### 2. Add explicit memory policy controls

Introduce additive policy/config controls for:

- enabling/disabling session memory
- enabling/disabling persistent memory
- enabling/disabling relevant-memory recall
- enabling/disabling team and agent scopes
- controlling read/write authorization by scope

### 3. Define deterministic scope precedence and conflict resolution

The feature must define how Quine resolves overlapping facts and lookup order across project, team, and agent scopes.

The first implementation should keep this precedence simple and explicit.

### 4. Respect trust and permission boundaries

Advanced scope behavior must integrate with Quine’s filesystem trust and permission model.

Memory reads and writes should be validated against the configured policy before they occur.

### 5. Add focused tests

Add tests for:

- scope resolution
- policy gating
- read/write authorization by scope
- precedence and conflict resolution
- prompt lookup or extraction behavior when multiple scopes are available

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Quine supports deterministic project, team, and agent durable-memory scopes when enabled.
- Memory policy and feature flags control read/write behavior by scope.
- Conflicts and lookup precedence across scopes are deterministic and test-covered.
- No shared inter-crate trait contract is modified to make this feature work.

## Non-Goals (Deferred)

- Rich interactive UI for cross-scope conflict management
- Distributed/shared remote memory coordination beyond local scope resolution
- Automatic merging of conflicting memories without explicit policy
