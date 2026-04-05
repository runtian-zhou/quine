# Render Session Fork History in CLI and TUI

Short summary: preserve parent/child fork relationships for sessions in the CLI-visible session model, and render that fork history in both the terminal UI and non-interactive CLI surfaces.

## Open Questions

- None at this time.

## Agreement Status

agreed — reviewed against the QA plan; both docs align on additive protocol support, shared tree reconstruction, CLI/TUI rendering, and validation coverage, with no unresolved open questions.

## Proposed Design

- Extend the session-listing data returned from the harness to expose enough ancestry metadata for clients to reconstruct a session fork tree without adding a second discovery round-trip.
- Reuse `quine-core` session tree concepts rather than introducing a parallel CLI-only hierarchy model; the harness should adapt persisted/core session ancestry into a stable protocol shape.
- Keep trait boundaries intact: this feature should be implemented within existing crate responsibilities, with protocol/data-model additions flowing from `quine-core`/`quine-harness` to `quine-cli` and no inter-crate trait redesign.
- Preserve existing flat session listing behavior where needed, but add a tree-aware rendering path for both the textual CLI session output and the TUI session browser/history view.
- Make rendering deterministic by sorting siblings consistently and handling missing ancestors defensively so older persisted sessions or partial data do not crash the client.

## File-by-File Changes

- `crates/quine-core/src/session_tree.rs`: confirm and, if necessary, extend the serializable session ancestry model so the harness can surface parent IDs, root IDs, depth, or ordered ancestor chains needed for rendering fork history.
- `crates/quine-core/src/engine.rs` and adjacent session persistence code: ensure forked sessions record ancestry in persisted state and that list/read paths can recover it for all sessions, not only the active branch.
- `crates/quine-harness/src/protocol.rs`: add wire-level fields to the session listing/session detail response types for ancestry metadata. Keep additions backward-compatible by using additive fields.
- `crates/quine-harness/src/service.rs`: populate the new protocol fields from core session state and preserve stable ordering for tree reconstruction.
- `crates/quine-cli/src/session.rs` and any related command handlers: update session-list/log commands to render a fork-aware history view in plain CLI output, including clear branch indentation and active-session markers where applicable.
- `crates/quine-cli/src/tui/mod.rs` and `crates/quine-cli/src/tui/app.rs`: add tree-aware state derived from the session list and render fork history in the TUI, ideally in the existing session sidebar/browser so users can inspect ancestry interactively without losing current behavior.
- CLI/TUI view-model helpers under `crates/quine-cli/src/`: add a minimal shared formatter or tree builder if both surfaces need the same ancestry transformation logic.
- Tests near the touched files: add unit tests for tree reconstruction, ordering, and rendering edge cases; add harness or CLI integration tests where current patterns already cover session-list output.

## Validation Plan

- Run focused unit tests for any new session-tree transformation helpers in `quine-core` and `quine-cli`.
- Run targeted CLI/harness tests covering session listing and any TUI state derivation logic added for the tree view.
- Run `cargo fmt --all -- --check`.
- Run `cargo clippy --all-targets -- -D warnings`.
- Run `cargo test` if targeted tests pass and the change touches shared session protocol/state paths.
- Perform at least one manual local daemon smoke test creating a forked session chain, then verify both CLI output and TUI rendering show the same ancestry structure.

## QA Feedback

- The implementation plan should explicitly keep the protocol change additive and backward-compatible so existing flat session consumers keep working while CLI/TUI opt into tree rendering.
- The validation plan should include one degraded-data case where a session references a missing parent so the rendering logic proves it will not panic on legacy or partial state.
- The current file-by-file scope matches the QA scenarios: core/harness expose ancestry metadata, CLI shares a tree builder/formatter, and TUI consumes the same derived hierarchy for consistent output.
