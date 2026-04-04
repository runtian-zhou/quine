---
status: done
---

# Shared Permission Evaluation Engine

Implement the deterministic shared permission-evaluation engine in `quine-core` that combines runtime mode, source-partitioned rules, tool-local input analysis, and headless prompt behavior into structured permission outcomes.

## Requirements

1. Add a shared evaluator in `crates/quine-core/src/permission/engine.rs`.
2. Define typed permission requests and structured outcomes.
3. Support precedence ordering:
   - tool-local hard deny
   - explicit deny rule
   - explicit allow rule
   - mode default
   - headless prompt fallback
4. Support tool-local `defer` semantics.
5. Preserve source attribution and stable explanation strings for later diagnostics.
6. Add narrow runtime integration without requiring full approval-routing UX.

## Acceptance Criteria

- Deterministic unit coverage for precedence, `defer`, source attribution, and headless prompt fallback.
- Structured `PermissionOutcome` values serialize cleanly.
- Narrow runtime-path integration invokes the shared evaluator before tool execution.
- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` pass.
