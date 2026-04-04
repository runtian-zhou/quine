---
status: done
---

# Permission Foundation and Runtime Context

## Overview

Implement the first permission-system slice by introducing Quine’s internal permission vocabulary and a session-scoped `PermissionContext` in `quine-core`.

This feature establishes foundational runtime state only. It initializes permission mode and related session permission metadata from existing bootstrap inputs without yet enforcing tool permissions, evaluating policies, or adding approval routing.

## Requirements

### 1. Add internal permission-domain types

Introduce additive internal permission types in `quine-core` for:

- permission modes
- permission decisions and rule effects
- rule sources and scopes
- permission targets and rules
- prompt behavior and approval-request identifiers

These types should remain crate-private unless broader visibility is required by existing integration seams.

### 2. Add session-scoped permission context

Add a `PermissionContext` runtime object owned by `SessionContext` that tracks:

- current permission mode
- optional pre-plan mode for later restoration
- source-partitioned permission rules
- workspace root
- additional allowed filesystem roots
- prompt behavior

The initial implementation should remain conservative and default-oriented.

### 3. Bootstrap from existing session inputs

Initialize permission context from current session bootstrap data only, especially:

- working directory
- existing `plan_mode` session creation input
- current interactive harness/CLI assumptions

Do not add new user-facing permission flags or RPC surfaces in this slice.

### 4. Centralize plan-mode bookkeeping

Route existing plan-mode exit handling through explicit permission-mode helpers so later permission work has a single runtime seam for mode transitions.

### 5. Add focused tests

Add tests covering:

- conservative default permission context initialization
- rule partitioning by source
- additional allowed-root mutation behavior
- plan-mode transition bookkeeping
- harness bootstrap coverage proving existing session creation reconstructs the expected permission state inputs

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Sessions initialize a `PermissionContext` from existing bootstrap inputs.
- Plan-mode exit uses centralized permission-mode transition helpers.
- No tool permission evaluation or approval workflow is introduced in this slice.
- No shared inter-crate trait contract is modified to make this feature work.

## Non-Goals (Deferred)

- permission rule evaluation and precedence resolution
- per-tool permission enforcement
- interactive approval request lifecycle
- new CLI or RPC permission configuration surfaces
