# 054 Render Session Fork History in CLI and TUI — QA Plan

Status: completed

## Checks

- Verify live session listings expose ancestry metadata.
- Verify `quine ps --tree` nests children under parents.
- Verify the TUI context explorer shows lineage details without crashing on root sessions.

## Evidence

- `cargo test -p quine-harness storage:: -- --nocapture`
- `cargo test -p quine-cli -- --nocapture`
