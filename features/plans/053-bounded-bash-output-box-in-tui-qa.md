# 053 Bounded Bash Output Box In TUI — QA Plan

Status: completed

## Checks

- Verify bash tool results render inside a boxed preview region.
- Verify long output is truncated instead of expanding the conversation indefinitely.
- Verify existing tool timing/status labels remain visible.

## Evidence

- `cargo test -p quine-cli -- --nocapture`
