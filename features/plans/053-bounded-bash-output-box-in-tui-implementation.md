# 053 Bounded Bash Output Box In TUI — Implementation Plan

Status: implemented

## Scope

- Keep the change inside `quine-cli` TUI rendering.
- Preserve existing tool status metadata while moving bash preview text into a bounded box.

## Delivered

- Bash tool results now store preview text in the TUI state.
- The conversation renderer draws bash preview output in a bordered box.
- The box wraps to the pane width and truncates long output to a small visible window.

## Validation

- `cargo test -p quine-cli -- --nocapture`
