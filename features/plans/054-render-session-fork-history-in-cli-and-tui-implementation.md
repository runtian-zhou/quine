# 054 Render Session Fork History in CLI and TUI — Implementation Plan

Status: implemented

## Scope

- Add lineage metadata to harness session listings and session-context snapshots.
- Render ancestry in `quine ps --tree` and in the TUI context explorer summary.

## Delivered

- Local harness session listings now include `parent_id`, `root_id`, and `depth`.
- Session context snapshots include lineage metadata.
- `quine ps --tree` prints parent/child relationships in a stable hierarchy.
- The TUI context explorer summary shows root, parent, depth, and child count.

## Validation

- `cargo test -p quine-harness storage:: -- --nocapture`
- `cargo test -p quine-cli -- --nocapture`
