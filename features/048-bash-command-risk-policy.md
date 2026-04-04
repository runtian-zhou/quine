---
status: pending
---

# Bash Command Risk Policy

Add deterministic command-risk analysis for Quine's `bash` tool so low-risk
inspection commands are not treated the same as mutating shell commands or
wrapper-based interpreter launches.

## Requirements

- Introduce a shared command analyzer in `quine-core` for `bash` requests.
- Classify representative commands into read-oriented, mutating, nested-shell,
  interpreter-launch, or conservative fallback buckets.
- Attach the analyzed command metadata to permission requests instead of
  treating shell execution as one undifferentiated execute permission.
- Use that metadata in the permission engine so low-risk shell inspection can
  follow a different default path than mutating or wrapper-launch commands.
- Add focused `bash` classifier tests plus evaluator coverage for differentiated
  outcomes.

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Read-oriented `bash` commands such as `pwd` are classified and permitted as
  lower-risk execution.
- Mutating shell commands remain gated more conservatively than read-only
  inspection commands.
- Nested shell wrappers and inline interpreter launches are classified into
  high-risk buckets and do not inherit read-only treatment.
