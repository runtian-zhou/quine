# Bounded Bash Output Box In TUI

Short summary: Render bash execution status stdout in the TUI inside a bounded, visually distinct box so large command output does not flush the full conversation area or dominate the screen.

## Open Questions

- None at this stage.

## Agreement Status

- Status: agreed
- Last reviewed QA plan revision: aligned with the latest QA scenarios covering long, wide, and failing bash output plus crate validation commands.

## Proposed Design

- Keep the change scoped to `quine-cli` TUI rendering and avoid altering cross-crate traits or transport contracts.
- Reuse the existing `ToolCall` / `result_preview` flow in the TUI state so the bounded presentation is purely a view concern.
- Update the tool result rendering path in `crates/quine-cli/src/tui/ui.rs` to draw bash execution output in a bordered sub-area instead of appending free-flowing preview lines directly into the main transcript paragraph.
- Constrain the rendered output by both width and height:
  - Width follows the conversation pane width and wraps within the box.
  - Height is capped to a small number of visible lines, with truncation or clipping that preserves the most useful content without expanding to full screen height.
- Apply the boxed treatment specifically to bash execution results that currently show stdout previews, while keeping non-bash tool rendering behavior unchanged unless the existing code path makes a generalized helper cleaner.
- Preserve any existing status metadata (success/failure, exit code, stderr summary if present) outside or in the box heading so the operator still sees execution outcome at a glance.

## File-by-File Changes

- `crates/quine-cli/src/tui/ui.rs`
  - Refactor the conversation rendering branch for tool result previews.
  - Add a helper that builds a boxed widget or boxed line block for bash output previews.
  - Enforce a maximum visible preview height and ellipsis/truncation behavior for oversized output.
  - Keep wrapping bounded to the inner width of the box.
- `crates/quine-cli/src/tui/app.rs`
  - Confirm the existing `result_preview` / execution summary generation already provides the data needed by the UI.
  - Only adjust preview shaping here if the UI cannot reliably distinguish bash output or needs a dedicated flag/source field that already exists in local types.
- `crates/quine-cli/src/tui/*.rs` tests (same-file unit tests if present, otherwise adjacent crate tests)
  - Add or extend rendering-focused tests for bounded bash output preview behavior if the current module has test patterns for text layout or conversation serialization.

## Validation Plan

- Run targeted tests covering TUI rendering code paths changed by the feature.
- Run `cargo test -p quine-cli` if targeted coverage exists there.
- Run `cargo clippy -p quine-cli --all-targets -- -D warnings`.
- Run `cargo fmt --all -- --check`.
- Perform a manual TUI smoke test with a bash command that emits many stdout lines and confirm:
  - the output is shown in a bordered box,
  - the box height remains bounded,
  - the surrounding conversation remains visible.

## QA Feedback

- The QA scenarios correctly focus on `quine-cli` and exercise the existing daemon + chat flow without requiring trait or protocol changes.
- The long-output and wide-output cases match the rendering risk in `crates/quine-cli/src/tui/ui.rs`, where preview text currently needs a bounded presentation.
- The failure scenario is important to ensure status/error text remains visible outside or alongside the boxed stdout preview.
- The implementation and QA plans agree that the change should stay view-local to the TUI unless a minimal `app.rs` tweak is needed to identify bash previews cleanly.
