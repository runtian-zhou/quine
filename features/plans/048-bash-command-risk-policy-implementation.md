# 048 Bash Command Risk Policy — Implementation Plan

Short summary: Add conservative command-aware risk analysis for the `bash` tool so shell execution is evaluated with richer metadata than a single undifferentiated execute permission.

## Open Questions

- None. This draft stays scoped to Feature 6 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Re-reviewed `features/plans/048-bash-command-risk-policy-qa.md` after its latest concrete revision. Both docs now align on deterministic `bash` command classification, evaluator metadata plumbing, exact test targets, and representative command strings, with no unresolved open questions.

## Proposed Design

- Build command-risk analysis specifically around the current `bash` tool implementation in `crates/quine-core/src/tool/bash.rs`.
- Keep the analyzer deterministic and explicit by introducing a `permission/command.rs` helper module rather than embedding ad hoc string inspection directly into the tool.
- Integrate at the request-construction seam rather than the shell-execution seam:
  - `bash.rs` should continue owning command invocation, timeout handling, and output capture
  - the new helper should classify the command immediately before permission evaluation, using the exact command string the tool will execute
  - `permission/engine.rs` should consume the richer metadata without learning shell parsing itself
- Mirror the existing core layering so each component keeps one responsibility:
  - `tool/bash.rs` builds the permission request from tool input plus execution context
  - `permission/request.rs` carries normalized command metadata as part of the request object shared with the evaluator
  - `permission/engine.rs` decides allow/ask/deny using that metadata and the current permission context
  - `engine.rs` continues orchestrating tool call lifecycle and reporting the eventual deny/prompt outcome through current session event paths
- Classify commands into a small first-release taxonomy aligned to current evaluator needs:
  - obviously read-oriented commands
  - potentially mutating commands
  - high-risk nested shell or interpreter launches
  - ambiguous commands that should remain conservatively high-risk
- Feed the analyzer output into the existing `PermissionRequest` shape instead of making the evaluator parse shell strings itself.
- Use the current working-directory and filesystem context only for additive metadata where it helps later diagnostics; avoid coupling command classification to runtime shell expansion or filesystem probing in this slice.
- Preserve the existing one-string `command` UX exposed by the tool while deriving a normalized representation for policy only:
  - classify off a conservative tokenization or prefix strategy
  - treat tokenization failures or unclear wrappers as high-risk/ambiguous rather than guessing
  - keep the executed shell string unchanged so this feature affects permission semantics, not command behavior
- Keep the first release conservative and table-driven:
  - simple prefix or argv-token classification is acceptable if it is explicit and well-tested
  - nested `sh -c`, `bash -lc`, `python -c`, and similar broad interpreter escapes should be treated as high-risk
  - broad launcher patterns should not inherit the same treatment as clearly bounded read-only commands
  - wrapper commands such as `env ... sh -c ...` or `python -c ...` should preserve enough normalized structure in metadata that later diagnostics can explain why they were treated as high risk
- Preserve compatibility with current `bash` tool behavior:
  - do not change shell execution semantics except for how permission metadata is constructed
  - do not rely on an LLM or runtime network classifier
  - leave finer-grained auto-mode stripping or policy rewriting for future slices

## File-by-File Changes

- `crates/quine-core/src/tool/bash.rs`
  - Add command-token extraction and request construction that attaches command-risk metadata before invoking the evaluator.
  - Preserve the current tool execution contract and error handling; only permission metadata and permission-facing rationale should change in this slice.
  - Keep the permission check positioned before the spawned shell command so denied high-risk requests fail without side effects.
- `crates/quine-core/src/permission/command.rs`
  - Add deterministic command classification helpers and risk categories.
  - Keep the classifier table-driven so later command-policy slices can extend it without rewriting `bash.rs`.
  - Expose a narrow helper API returning normalized command facts that `request.rs`, diagnostics, and tests can all share.
- `crates/quine-core/src/permission/request.rs`
  - Extend request metadata to carry shell command text, normalized argv/prefix information, and command-risk classification.
  - Keep fields additive so diagnostics and persisted-rule features can later inspect command-oriented requests without changing the `bash` tool again.
  - Ensure the request shape stays general enough that future non-`bash` execute tools could reuse the same command-risk fields.
- `crates/quine-core/src/permission/engine.rs`
  - Consume richer `bash` request metadata without embedding shell parsing logic into the evaluator.
  - Use the classification to differentiate low-risk and high-risk outcomes under the same rule set while keeping precedence and mode handling centralized here.
  - Keep explanation data aligned with Feature 051 so deny or ask results can surface the command-risk reason instead of an opaque generic execute denial.
- `crates/quine-core/src/engine.rs`
  - Reuse the current tool orchestration flow so command-risk denials still surface through the existing tool-result/session-error pathways.
  - Avoid special-casing `bash` in session orchestration beyond consuming richer permission outcomes.
- Colocated and integration tests
  - Add table-driven classification tests and representative evaluator interaction coverage.
  - Keep focused tests near `permission/command.rs` and broader request/evaluator coverage in existing `quine-core` integration patterns.

## Validation Plan

- Table-driven unit tests for command classification:
  - read-like commands such as `pwd`, `ls`, or equivalent simple inspection commands
  - write-capable commands such as file creation or modification patterns
  - nested-shell and interpreter-launch patterns such as `sh -c`, `bash -lc`, and `python -c`
  - ambiguous commands that intentionally resolve to conservative/high-risk buckets
- Regression tests for risky wrappers:
  - ensure broad interpreter or shell-launch patterns do not get misclassified as simple read-only commands
- Integration tests with the shared evaluator:
  - the same policy setup can distinguish a clearly safe command from a high-risk command because the metadata differs
  - denials and prompts surface reasons tied to the command-risk classification rather than opaque execute-scope text
  - request-building tests should prove `bash.rs` and `permission/command.rs` agree on the classification inputs they exchange
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Re-reviewed `features/plans/048-bash-command-risk-policy-qa.md` after its latest revision.
- The QA plan now matches this implementation plan’s concrete interaction points with existing code:
  - focused classifier tests around the new `permission/command.rs` helper
  - request/evaluator coverage proving `tool/bash.rs` and `permission/engine.rs` consume the same normalized metadata
  - daemon-backed `bash` scenarios using representative commands such as `pwd`, file-mutating commands, and nested shell or interpreter launches
- Scope remains aligned: deterministic classification in `quine-core`, additive request metadata, unchanged shell execution semantics, and conservative treatment of ambiguous commands.
- No further QA-side changes are required from this review.
