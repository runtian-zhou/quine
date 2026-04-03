# 040 Prompt-Time Persistent Memory Injection and Recall — Implementation Plan

Short summary: Extend Quine prompt construction to support bounded baseline `MEMORY.md` injection and deterministic targeted durable-memory recall, while keeping the feature internal to `quine-core`, additive to existing session inspection, and behaviorally unchanged when disabled.

## Open Questions

- None from implementation planning at this stage. This slice can stay limited to prompt-time baseline index injection plus deterministic targeted recall for project-scoped persistent memory, with richer diagnostics, advanced scopes, and policy controls explicitly deferred.

## Agreement Status

agreed — I reviewed the latest paired QA doc revision after it was completed, and the two docs now align on scope, required deterministic unit/integration coverage, the required multi-round local-daemon test, additive-only inspection, and no unresolved open questions.

## Proposed Design

- Scope this work to the Feature 4 slice from `docs/design/002-memory-systems-design.md` only:
  - baseline prompt-time `MEMORY.md` injection
  - deterministic targeted relevant-memory recall
  - bounded prompt budgeting and truncation behavior
  - additive inspection of what prompt-time memory injection ran
- Keep the implementation internal and avoid shared inter-crate trait changes:
  - `quine-core` owns prompt-memory models, selection heuristics, file loading, budget enforcement, and final prompt assembly.
  - `quine-harness` and `quine-cli` only receive additive session-context visibility if needed to inspect the last prompt-memory injection.
  - `quine-sdk` and shared orchestration traits remain unchanged.
- Build on the current prompt path in `crates/quine-core/src/engine.rs`, where `build_combined_system_prompt(...)` creates the baseline system prompt and `SessionContext::new(...)` seeds the system message into `history`.
- Introduce a dedicated internal persistent-memory prompt module under `crates/quine-core/src/memory/` rather than expanding `engine.rs` inline with parsing and ranking logic.
- Reuse the durable store introduced by Feature 039 rather than inventing a new storage layout. The source of truth stays on disk under the existing project-scoped memory directory, with `MEMORY.md` as an index and one durable memory per markdown file.
- Preserve disabled-mode behavior by representing prompt memory injection explicitly with an internal mode enum whose default path is a no-op.

### Planned internal model and helpers

- Add internal prompt-memory types in `crates/quine-core/src/memory/` aligned with the design doc, but scoped only to what this slice needs:
  - `PromptMemoryInjectionMode` with at least `Disabled`, `IndexOnly`, and `TargetedRecall`
  - `RelevantMemoryBudget` for bounded counts/bytes/chars used by both index injection and targeted recall
  - `RelevantMemoryCandidate` for deterministic scoring inputs derived from entry metadata
  - `RelevantMemorySelection` for ranked/selected/skipped results and truncation accounting
  - `PromptMemoryEnvelope` for separating baseline prefix text from turn-local reminder messages
  - `PromptMemoryInjection` for the exact per-turn memory payload plus an inspectable summary of what happened
- Keep `last_prompt_memory_injection` runtime-only in `SessionContext`; do not persist it in checkpoints for this feature.
- Plan helpers closely following the design doc’s decomposition:
  - load `MEMORY.md` from the resolved persistent-memory paths when index injection is active
  - load durable memory records for recall using existing persistent-memory metadata/index support where available
  - extract the latest user text from current history for turn-local recall
  - score and rank candidates deterministically using only local heuristics
  - read full markdown bodies only for entries that survive selection
  - render selected entries into synthetic reminder messages
  - splice prompt-memory output into the final request without mutating persisted conversation history

### Injection modes and prompt assembly

- Split prompt construction into ordered internal stages before each LLM call:
  1. build the normal baseline prompt material exactly as today
  2. derive a `PromptMemoryInjection` from persistent-memory state, injection mode, and the latest user turn
  3. construct the final request messages by adding baseline memory prefix and/or targeted reminder messages in deterministic positions
- `IndexOnly` mode:
  - load bounded `MEMORY.md` content
  - append it to the same system-prompt-style prefix path already used for `CLAUDE.md` and system prompt content rather than emitting extra transcript messages
  - include an explicit stale-memory caveat reminding the model to verify durable facts against the repository and current user instructions
- `TargetedRecall` mode:
  - do not implicitly include broad `MEMORY.md` index text by default
  - instead, inject zero or more synthetic system-side reminder messages immediately before the newest user message in the provider request
  - keep these reminder messages ephemeral for the turn only; do not append them to `SessionContext.history`
  - if there is no latest user text or no candidates above the threshold, inject nothing rather than silently falling back to baseline index injection
- `Disabled` mode:
  - produce no prompt-memory material and preserve current prompt behavior bit-for-bit aside from additive internal bookkeeping that remains empty

### Deterministic selection and budgeting

- Keep the first release’s recall strategy deterministic and explainable, using only metadata and text overlap heuristics already allowed by the feature request.
- Candidate scoring should consider:
  - overlap between normalized latest user text and memory title/summary/keywords
  - `pinned` entries receiving a stable boost
  - recency via `updated_at`
  - project-scope match only for this feature
  - stale entries either excluded or heavily de-prioritized, with the behavior chosen once and made explicit in tests
- Deterministic ordering for selected targeted memories should be:
  - highest score first
  - ties broken by `pinned`
  - then most recent `updated_at`
  - then stable `entry_id`
- Budgeting should be centralized in `RelevantMemoryBudget` and apply to both modes:
  - cap index injection size so `MEMORY.md` cannot grow without bound in prompts
  - cap the number of targeted entries per turn
  - cap per-entry reminder body size with deterministic truncation
  - record which entries were skipped for budget, threshold, stale, or duplication reasons so diagnostics and tests can explain selection behavior
- Repeated prompt builds within one live turn/session should de-duplicate by `entry_id` using transient runtime bookkeeping only.

### Runtime integration boundaries

- Extend `SessionContext` in `crates/quine-core/src/engine.rs` with additive prompt-memory runtime state only, for example:
  - resolved persistent-memory configuration/state from the existing durable memory feature
  - injection mode and budget configuration
  - `last_prompt_memory_injection` summary for inspection/debugging
  - transient `last_selected_entry_ids` to avoid duplicate turn-local reminder selection when prompt construction re-runs
- Keep prompt mutation internal to `quine-core::engine`; do not add a new prompt-builder trait or modify existing core orchestration traits.
- Add a narrow internal prompt-builder helper in `engine.rs` so model invocation uses ephemeral request messages assembled from:
  - persisted history
  - baseline system prompt content
  - optional prompt-memory injection
- Keep session restore/checkpoint semantics simple:
  - persistent-memory files remain on disk as the durable source
  - prompt-memory diagnostic state remains runtime-only unless an existing inspection path requires serializing a small additive summary

### Configuration and scope handling

- Reuse persistent-memory enablement and resolved project-scoped paths from the durable memory store feature rather than inventing a parallel path resolver.
- Keep scope limited to project memory only for this feature, even if the internal types are future-friendly enums.
- If the codebase does not yet expose a dedicated session-level injection mode, add it as additive internal/harness config rather than a cross-crate trait change. The minimum viable control surface is:
  - disabled by default unless the existing memory feature gate enables it
  - internal ability to select `IndexOnly` vs `TargetedRecall` for tests and future CLI plumbing
- Defer all advanced scope routing, team memory, agent-specific memory, and policy-rich selection controls to Feature 042.

### Additive inspection surface

- The feature request allows additive diagnostics if they explain which injection mode ran and which memory entries were selected.
- The lightest-weight plan is to extend the existing session context inspection path used by `GET_SESSION_CONTEXT` with additive prompt-memory fields, such as:
  - whether prompt memory was disabled, skipped, or used
  - which mode ran
  - selected entry ids/titles
  - skip reasons and truncation flags
- Keep this inspection read-only and summary-oriented. Do not introduce a new RPC method or UI workflow in this feature.
- Coordinate the exact shape with the later diagnostics feature by keeping the prompt-memory summary private to `quine-core` and serializing only a minimal snapshot outward for now.

## File-by-File Changes

- `crates/quine-core/src/lib.rs`
  - Re-export any new internal memory module pieces needed by existing crate boundaries, but keep public surface area minimal.
  - Avoid widening public API unless the harness inspection path truly needs a small exported summary type.
- `crates/quine-core/src/engine.rs`
  - Refactor current prompt assembly so provider-facing request messages can be built from history plus ephemeral prompt-memory injection.
  - Extend `SessionContext` with additive runtime prompt-memory state.
  - Initialize prompt-memory state on session creation and restore.
  - Record the latest injection summary each time the model is invoked.
  - Add focused unit tests near prompt construction covering disabled mode, index-only ordering, and targeted reminder placement.
- `crates/quine-core/src/memory/mod.rs`
  - Add the internal memory module entrypoint if Feature 039 has not already created one.
  - Wire submodules for paths/loading, records, selection, and rendering in a crate-private layout.
- `crates/quine-core/src/memory/prompt.rs`
  - Add prompt-time models such as `PromptMemoryInjectionMode`, `RelevantMemoryBudget`, `PromptMemoryInjection`, and selection result types.
  - Implement helpers for loading index markdown, selecting relevant records, truncating selected entries, and rendering reminder messages.
  - Keep parsing/ranking deterministic and heavily unit-tested in this module.
- `crates/quine-core/src/memory/store.rs` or existing Feature 039 memory files
  - Reuse or extend existing functions that enumerate memory records and resolve project-scoped memory paths.
  - Add any missing lightweight record-loading helper needed for targeted recall without changing the durable storage format.
- `crates/quine-core/src/persistence.rs`
  - Only change this file if a tiny additive persisted config field is required to restore a session’s prompt-memory mode; otherwise leave checkpoint formats unchanged.
  - Do not persist `last_prompt_memory_injection` or selected entry payloads.
- `crates/quine-harness/src/config.rs`
  - Additive session or harness config fields only if needed to select prompt-memory injection mode/budget for tests and future wiring.
  - Keep defaults conservative so current behavior remains unchanged unless explicitly enabled.
- `crates/quine-harness/src/local.rs`
  - Thread any new additive config values into `CoreInput::CreateSession` construction if the feature needs session-scoped enablement.
  - Preserve current create-session behavior as the default path.
- `crates/quine-harness/src/server.rs`
  - Parse any additive `create_session` parameters only if needed for explicit enablement in tests/QA.
  - Keep the existing RPC method and backward compatibility intact.
- `crates/quine-harness/src/storage.rs`
  - Extend `SessionContextSnapshot` with additive prompt-memory summary fields if session inspection is used to expose selection/mode information.
  - Keep snapshot changes backward compatible and read-only.
- `crates/quine-cli/src/context_debug.rs`
  - Update the debug/session-context snapshot struct only if harness inspection adds prompt-memory fields.
  - No new CLI command is required; existing context rendering should naturally show the extra JSON fields.
- `crates/quine-core/tests/` or adjacent integration tests
  - Add integration coverage if prompt construction behavior is easier to verify outside `engine.rs` unit tests, especially for end-to-end request assembly and restore-safe disabled behavior.

## Validation Plan

- Add unit tests in `crates/quine-core/src/memory/prompt.rs` for:
  - latest-user-text extraction
  - deterministic candidate scoring and tie-breaking
  - pinned/recency/keyword overlap ranking behavior
  - stale-entry handling behavior
  - budget capping and deterministic truncation
  - empty-selection behavior when no relevant memory qualifies
- Add unit tests in `crates/quine-core/src/engine.rs` for prompt construction integration:
  - disabled mode leaves provider request/history behavior unchanged
  - index injection appears in deterministic prefix order after the base prompt material
  - targeted reminder messages are inserted before the newest user message and not persisted into session history
  - targeted recall does not implicitly fall back to index injection when nothing matches
- Add harness/session-context tests if additive inspection fields are introduced:
  - session context snapshot includes the last prompt-memory mode/result summary
  - disabled/skipped/succeeded states serialize predictably
- Expect the eventual implementation PR to run full workspace verification:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
- QA should also be able to execute at least one deterministic local-daemon multi-round scenario once implementation lands, because this feature changes `quine-core` prompt construction.

## QA Feedback

- I re-reviewed the latest completed QA plan revision and it now covers the required executable scope for this feature slice.
- Agreement is no longer blocked. The QA plan now concretely specifies:
  - focused `quine-core` unit coverage for `IndexOnly` ordering, deterministic ranking, truncation, no-match behavior, and disabled-mode equivalence
  - `quine-core` integration coverage proving targeted reminder messages are injected before the latest user message and do not persist into `SessionContext.history`
  - a required local-daemon multi-round test with exact commands, exact user messages, and exact expected final responses / status / tool activity
  - additive-only session-context evidence rather than expanding into the deferred diagnostics feature
- The paired docs are aligned on the key implementation/QA contracts:
  - prompt-time baseline `MEMORY.md` injection plus deterministic targeted project-scoped recall only
  - no fallback from `TargetedRecall` to broad index injection on no-match turns
  - centralized bounded budgeting and deterministic truncation
  - disabled mode remains a true no-op for prompt construction
  - no shared inter-crate trait contract changes are required
- No further QA-side changes are required before implementation begins.