---
status: pending
---

# Filesystem Permission Boundaries

Add shared path-based authorization in Quine's permission layer so filesystem
tools evaluate resolved targets against the workspace root and any additional
approved roots before execution.

## Requirements

- Add a shared path authorization helper in `quine-core`.
- Evaluate final resolved paths, not only raw lexical input.
- Deny targets outside the workspace root and additional approved roots.
- Apply the same boundary behavior to `read_file`, `find`, and `apply_patch`
  through shared permission evaluation.
- Keep outside-root writes fail-closed with deterministic permission-denied
  behavior.
- Add focused tests for workspace, additional-root, traversal, symlink, and
  runtime outside-root denial coverage.

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Outside-root file requests are denied by permission evaluation before tool
  execution.
- In-bounds paths remain allowed under the existing permission mode contract.
