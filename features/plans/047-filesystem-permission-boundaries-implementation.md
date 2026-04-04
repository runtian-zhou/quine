# 047 Filesystem Permission Boundaries — Implementation Plan

Short summary: Centralize path-based authorization for Quine filesystem tools by adding canonical workspace/additional-directory boundary evaluation, read/write distinction, and sandbox-aware path checks in the shared permission layer.

## Open Questions

- None. This draft stays scoped to Feature 5 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Re-reviewed `features/plans/047-filesystem-permission-boundaries-qa.md` after its latest concrete revision. Both docs now align on shared `quine-core` path authorization, exact unit/integration coverage, the required daemon-backed multi-round scenario, and there are no unresolved open questions.

## Proposed Design

- Build shared path policy on top of the filesystem abstractions that already exist:
  - `SessionFilesystem` in `crates/quine-core/src/filesystem/mod.rs` already exposes `resolve_path()` and `root()`
  - file-oriented tools already operate through `ExecutionContext.filesystem` and `ExecutionContext.working_directory`
  - current filesystem errors already include `PermissionDenied` and `PathTraversal`, which provide natural integration points for permission denials and boundary failures
  - the new helper should stay beneath tool implementations so `read`, `find`, and `write` continue to present the same tool-facing contract while delegating boundary evaluation to one shared place
- Add a permission/path helper layer in `crates/quine-core/src/permission/path.rs` rather than duplicating containment logic across `read.rs`, `find.rs`, and `write.rs`.
- Keep this helper internal-first and pure where possible:
  - accept the existing session filesystem root, working directory, scope, and requested path(s)
  - return a deterministic authorization result plus the resolved target path used for diagnostics in later features
  - avoid pushing path-containment logic up into `quine-harness` or `quine-cli`
- Normalize boundary evaluation around final resolved paths:
  - use `SessionFilesystem::resolve_path()` first
  - compare the resolved absolute target against the session workspace root plus any additional allowed roots from `PermissionContext`
  - distinguish read-scoped versus write-scoped checks while keeping the containment primitive shared
- Keep harness involvement narrow and bootstrap-focused:
  - `SessionConfig` / `CoreInput::CreateSession` may later carry additive allowed-root or sandbox data
  - this slice should consume those inputs if they exist, but the containment engine should remain in `quine-core`
  - if current harness startup already knows a workspace root or additional approved roots, thread only normalized path data into core rather than reimplementing authorization at the daemon boundary
- Make edge-case behavior explicit and deterministic:
  - paths inside `filesystem.root()` are in-bounds by default unless future rules narrow them
  - additional approved roots are additive and should be normalized once before evaluation
  - traversal attempts are judged on resolved targets, not raw `..` string presence alone
  - symlink escapes should be denied when the resolved target lands outside all approved roots
  - outside-root writes should always fail closed
- Reuse the same helper from current file tools:
  - `read.rs` for file reads
  - `find.rs` for search roots and traversal boundaries
  - `write.rs`/`apply_patch` for target edits and file creations
- Leave shell command and broader sandbox semantics to later slices:
  - this feature focuses on path authorization, not command classification
  - sandbox-derived allowlists should only be threaded in if the existing harness runtime can already supply them coherently

## File-by-File Changes

- `crates/quine-core/src/permission/path.rs`
  - Add root normalization, resolved-path containment checks, and read/write authorization helpers.
- `crates/quine-core/src/permission/context.rs`
  - Ensure `PermissionContext` can expose workspace root and additional allowed roots in a form usable by path policy helpers.
  - Keep the data model compatible with the permission design doc’s `additional_roots` concept so later diagnostics and persisted-rule features can inspect the same state without redefining it.
- `crates/quine-core/src/filesystem/mod.rs`
  - Reuse existing `resolve_path()` / `root()` APIs; only adjust interfaces if a minimal additive helper is necessary for deterministic authorization.
  - Preserve the current separation where filesystem resolution stays here and policy decisions stay in the permission subsystem.
- `crates/quine-core/src/tool/read.rs`
  - Route target path authorization through the shared path helper before actual reads.
  - Keep existing read output formatting unchanged; only the allow/deny decision path should change.
- `crates/quine-core/src/tool/find.rs`
  - Route search-root and traversal authorization through the shared path helper.
  - Ensure recursive traversal never walks into a resolved path outside approved roots even if the user-supplied starting path looked in-bounds lexically.
- `crates/quine-core/src/tool/write.rs`
  - Route patch target authorization through the shared path helper before writes/edits occur.
  - Cover both edits to existing files and `new_file_content` creation paths so outside-root writes fail before any file mutation happens.
- `crates/quine-core/src/permission/request.rs`
  - Ensure filesystem-oriented permission requests carry enough resolved-path/root metadata for later diagnostics.
  - Keep the request shape additive so later Features 051 and 052 can expose source/reason details without revisiting every filesystem tool.
- `crates/quine-harness/src/config.rs` and create-session bootstrap path
  - Thread additional allowed roots or sandbox-derived roots only if the current harness startup model already has a trustworthy source for them.
  - Otherwise, keep the first implementation limited to the workspace root plus any existing in-memory session additions so the feature lands without speculative config surface changes.
- Colocated and integration tests
  - Add containment, traversal, symlink, and outside-root regression coverage.

## Validation Plan

- Unit tests for shared path helpers in `quine-core`:
  - resolved target inside workspace root is allowed according to scope
  - resolved target inside an additional approved root is treated as in-bounds
  - resolved target outside all approved roots is denied
  - traversal input that resolves in-bounds succeeds while traversal that resolves out-of-bounds fails
- Symlink/escape tests where supported by the environment:
  - a symlink whose resolved target escapes the workspace/additional roots is denied
  - helper behavior is based on the final resolved target, not the symlink’s lexical location
- Tool-level integration tests:
  - `read.rs` and `find.rs` honor the same shared boundary logic
  - `write.rs` denies outside-root edits even if the raw user path appears superficially in-bounds
- Negative regression tests:
  - outside-root write attempts fail closed
  - inconsistent tool-specific path checks do not reappear after shared helper adoption
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Re-reviewed `features/plans/047-filesystem-permission-boundaries-qa.md` after its latest revision.
- The QA plan now satisfies the feature-planning workflow’s concreteness requirements:
  - exact focused `cargo test` selectors are defined for workspace-root, additional-root, outside-root, traversal, and symlink cases
  - the shared-helper integration coverage now names a concrete `quine-core` integration test entry point for `read_file`, `find`, and `apply_patch`
  - the required daemon-backed `quine-core` scenario now includes the exact daemon startup command, exact round-by-round user messages, and explicit expected tool activity and allow/deny outcomes
- Scope remains aligned with this implementation plan: shared path-policy enforcement in `quine-core`, additive additional-root modeling, final resolved-target evaluation, and deterministic outside-root denial without expanding into shell-risk classification.
- No further QA-side changes are required from this review.
