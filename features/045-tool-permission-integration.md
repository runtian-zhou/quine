---
status: done
---

# Tool Permission Integration

Integrate Quine's shared permission engine with the currently exposed built-in
tools so runtime execution consistently classifies requests before the tool
runs.

## Requirements

- Build permission requests for current built-in tools using their actual tool
  names and resolved resources.
- Preserve conservative defaults for mutating, execution, and agent-control
  tools.
- Keep internal interaction surfaces such as `ask_user` on the normal
  interaction path instead of treating them as external side effects.
- Record permission outcomes before tool execution so later diagnostics and
  approval routing features have stable runtime state to inspect.
- Add focused tests for representative read, write, and interaction
  classifications.

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- `apply_patch` is classified as a write-scoped tool using its target path.
- `find` and `read_file` produce read-scoped path requests.
- `ask_user` remains usable without entering a permission-denial loop.
- Permission outcomes are recorded before tool execution.
