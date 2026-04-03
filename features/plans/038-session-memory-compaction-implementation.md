# 038 Session-Memory-Driven Compaction — Implementation Plan

Short summary: Integrate session memory into Quine compaction so `summary.md` plus boundary metadata can drive compacted history when valid, while preserving the existing legacy compaction path as a safe fallback.

## Open Questions

- None from implementation planning at this stage. The design can stay entirely inside `quine-core` by treating session memory as an internal compaction input and by using bounded coordination with in-flight refresh work before falling back to the legacy summarizer path.

## Agreement Status

agreed — reviewed the latest QA-plan revision after it was completed; both docs now align on internal-only `quine-core` compaction changes, bounded refresh coordination, safe fallback to the legacy summarizer, required archive/live-tail/tool-result invariants, and the concrete daemon-backed and in-process validation coverage needed for execution, with no unresolved open questions.

## Proposed Design

- Scope this feature to Feature 2 in `docs/design/002-memory-systems-design.md`: compaction consumes the session-memory summary when safe, but session memory creation, prompt-time durable-memory injection, and persistent cross-session memory remain out of scope.
- Keep all behavior internal to `crates/quine-core`; do not modify shared inter-crate traits or external CLI / SDK / harness protocols.
- Preserve the current compaction ownership model:
  - `engine.rs` still decides when compaction runs
  - `compaction.rs` still archives the full pre-compact transcript
  - the only behavior change is how the replacement compact summary is chosen
- Add an internal compaction decision layer that tries session-memory compaction first and falls back to the existing LLM summarizer flow when session-memory state is unavailable or unsafe.
- Reuse the Feature 1 session-memory foundation rather than inventing a second continuity store. The compaction path should consume:
  - `summary.md`
  - summary metadata / boundary state
  - additive runtime state on `SessionContext`
- Use index-based transcript boundaries against the canonical in-memory `session.history`; do not introduce new message-id contracts or cross-crate identifiers.

### Planned internal types and decisions

- Add or confirm an internal `CompactionSource` enum in `quine-core` with:
  - `SessionMemory`
  - `LegacySummarizer`
- Add a session-memory compaction input type, e.g. `SessionMemoryCompactionInput`, carrying:
  - loaded `summary.md` text
  - parsed summary metadata
  - resolved boundary classification
- Add a boundary model that lets compaction distinguish the safe path from fallback cases, for example:
  - `SessionMemoryBoundary::Indexed { last_summarized_message_index }`
  - `SessionMemoryBoundary::ResumedSession`
  - `SessionMemoryBoundaryResolution::{Ready, MissingSummary, TemplateOnly, MissingMetadata, InvalidBoundary, RefreshInFlight}`
- Add an internal `CompactionPlan` wrapper so the decision about which source to use is made once and then applied consistently to history replacement and state bookkeeping.
- Extend additive per-session memory state only where compaction needs durable bookkeeping, such as:
  - `last_compaction_source: Option<CompactionSource>`
  - `last_compacted_message_index: Option<usize>`
- Persist only restore-relevant compaction metadata in checkpoints. Do not persist raw summary contents into checkpoints; `summary.md` remains disk-backed.

### Compaction flow changes

- Keep the current high-level sequence in `engine.rs::compact_session_history(...)`:
  1. split history into compactable prefix and live tail
  2. archive the full transcript before replacement
  3. choose a compact summary source
  4. rebuild compacted history
  5. update in-memory session state
- Replace the current unconditional `summarize_history(...)` call with a two-stage selection flow:
  1. inspect session-memory state and attempt to load a consistent session-memory snapshot
  2. if that yields a safe compaction plan, use it; otherwise call the existing legacy summarizer path unchanged
- Preserve current invariants exactly:
  - system message stays at the front when present
  - transcript archives are still emitted under the existing archive root
  - the live tail remains preserved based on existing `live_tail_start(...)` behavior
  - archived tool results continue to behave exactly as they do today
  - a failed session-memory path must not make compaction fail if the legacy path would have succeeded

### Session-memory readiness rules

- Session-memory compaction should activate only when all of the following are true:
  - the session has session-memory state enabled / initialized
  - `summary.md` exists and is not just the template / empty shell
  - metadata parses successfully
  - the boundary resolves to a safe cut within the compactable prefix
  - the resulting unsummarized tail can be preserved without violating current compaction invariants
- Treat the following as immediate fallback conditions rather than hard errors:
  - missing summary file
  - missing metadata sidecar or absent summarized boundary when the state is otherwise not usable
  - unreadable or malformed metadata
  - stale boundary beyond current history length
  - boundary that would cut through the preserved live tail region
  - any failure while constructing the session-memory summary message
- Support a resumed-session mode only if Feature 1 state already distinguishes “summary exists, but there is no reliable summarized boundary yet.” In that case the plan should explicitly preserve the current transcript as tail where needed rather than risk dropping content. If the implementation cannot compute that safely, it must fall back.

### Coordination with in-flight refresh work

- Compaction must consume a consistent snapshot of session-memory state.
- Prefer additive coordination through existing `SessionContext.session_memory` runtime fields rather than a new cross-crate interface.
- If a summary refresh is currently in flight:
  - first attempt a bounded wait only if the runtime already exposes a concrete join handle / completion primitive that can be awaited without broad architectural churn
  - otherwise fall back immediately to the legacy summarizer path
- Do not block compaction indefinitely waiting for summary maintenance.
- Any coordination failure, timeout, or cancelled refresh should degrade to the legacy path, not surface as a user-visible compaction failure.

### Resulting compacted history shape

- Keep the current compacted history format centered on:
  - the original system message, if present
  - one compact assistant summary message referencing the archive path
  - the preserved live tail messages
- For the first implementation, reuse the existing `compacted_history(...)` message shape and only change the summary payload content source. This keeps downstream behavior stable while improving compacted continuity.
- The session-memory-derived summary text should be built from the stored `summary.md` contents, potentially prefixed with a short marker that it came from maintained session memory only if that can be done without breaking current expectations. If there is any risk of churn, preserve the current wrapper string and swap only the body text.

### State and restore behavior

- Extend `SessionContext` with only the additive runtime fields needed for compaction bookkeeping and refresh coordination.
- Extend `PersistedSession` with additive, defaultable memory-state fields only if restore needs the previous compaction source or last compacted boundary to resume safely.
- Keep backward compatibility with older checkpoints by making all new persisted fields optional / defaultable.
- Never require the summary file contents to be embedded in checkpoints. On restore, the runtime should recompute paths and reload disk-backed session-memory state best-effort.
- If persisted memory-compaction metadata is missing during restore, treat the session as eligible only for legacy compaction until fresh session-memory state is available.

### Implementation phases and dependencies

- Phase 1 — add internal compaction selection types and helper seams.
  - Dependency: existing compaction and session-memory foundation code.
  - Outcome: compaction can represent “session memory vs legacy summarizer” without changing behavior yet.
- Phase 2 — add session-memory loading and boundary resolution helpers.
  - Dependency: Phase 1 helper/types.
  - Outcome: compaction can determine whether session-memory input is safe and where the unsummarized tail begins.
- Phase 3 — wire the new decision model into `engine.rs::compact_session_history(...)`.
  - Dependency: Phase 2 readiness logic.
  - Outcome: compaction prefers session memory when ready and falls back cleanly when not.
- Phase 4 — add checkpoint / restore bookkeeping only if needed for resumed-session correctness and diagnostics.
  - Dependency: concrete restore need identified during implementation.
  - Outcome: restart behavior stays safe without forcing a new external API.
- Phase 5 — add focused unit and integration regression coverage for both success and fallback paths.
  - Dependency: completed implementation wiring.

### Risks and mitigations

- Risk: dropping transcript content by trusting an invalid boundary.
  - Mitigation: require boundary validation against in-memory history length and existing live-tail cutoff before using session memory.
- Risk: racing compaction against a background summary refresh.
  - Mitigation: consume only a consistent snapshot, prefer bounded wait when cheaply available, otherwise fall back.
- Risk: changing the visible compacted history shape too much and breaking existing assumptions.
  - Mitigation: preserve the current wrapper message structure and archive generation flow.
- Risk: introducing restore fragility through over-persisted state.
  - Mitigation: persist only additive, optional metadata and keep `summary.md` disk-backed.
- Risk: expanding scope into broader memory features.
  - Mitigation: keep all changes isolated to session-memory-driven compaction and explicit fallback handling.

## File-by-File Changes

- `crates/quine-core/src/compaction.rs`
  - Add crate-private compaction decision types such as `CompactionSource`, `CompactionPlan`, and session-memory boundary/result enums if they fit best here.
  - Add helper functions for:
    - loading a compaction-ready session-memory snapshot
    - resolving a safe summarized boundary against `history`
    - building a compact summary message from `summary.md`
    - building / applying the chosen compaction plan
  - Keep existing legacy helper functions intact so the fallback path remains the current code path, not a reimplementation.
  - Preserve `archive_history(...)`, `split_history_for_compaction(...)`, `compacted_history(...)`, and tool-result archiving invariants.

- `crates/quine-core/src/engine.rs`
  - Update `compact_session_history(...)` so it queries session-memory readiness before invoking `summarize_history(...)`.
  - Thread the session-memory runtime state already attached to `SessionContext` into the compaction decision logic.
  - Coordinate with any in-flight session-memory refresh state using bounded wait or immediate fallback semantics.
  - Record additive compaction bookkeeping such as `last_compaction_source` and `last_compacted_message_index` after compaction succeeds.
  - Keep auto-compaction triggers, transcript archive creation, and legacy summarizer calling behavior otherwise unchanged.

- `crates/quine-core/src/memory/session.rs`
  - Extend the internal session-memory runtime structs introduced by Feature 1 with compaction-relevant state and helper methods.
  - Provide helpers for reading `summary.md` plus metadata as a consistent compaction input.
  - Provide boundary validation against current transcript length and any restore-safe persisted markers.
  - Expose only crate-private helpers; do not create a new public memory interface.

- `crates/quine-core/src/persistence.rs`
  - Add additive persisted session-memory compaction metadata only if implementation proves it is necessary for resumed-session correctness or restore-safe diagnostics.
  - Keep serde defaults / backwards compatibility so older checkpoints still deserialize.
  - Avoid persisting raw markdown summary contents.

- `crates/quine-core/src/lib.rs`
  - Re-export or declare any new crate-private modules needed for compaction / memory integration, if the Feature 1 structure requires it.

- `crates/quine-core/tests/` integration coverage
  - Add or extend end-to-end compaction tests that exercise:
    - session-memory preferred path
    - fallback to legacy summarizer
    - invalid boundary rejection
    - live-tail preservation
    - archive generation and tool-result regression behavior
    - refresh-in-flight coordination behavior

- `crates/quine-core/src/compaction.rs` unit tests (or adjacent module tests)
  - Add focused tests for boundary resolution, decision selection, summary loading validation, and plan application.

## Validation Plan

- Unit-test the session-memory decision logic as close to the owning helpers as possible:
  - valid summary + valid boundary chooses `CompactionSource::SessionMemory`
  - missing summary / template-only summary falls back
  - malformed metadata falls back
  - stale or out-of-range boundary falls back
  - boundary intersecting the preserved live tail falls back
  - resumed-session / boundaryless state only succeeds when the preserved result is unambiguously safe
- Unit-test compacted history construction:
  - system message remains first
  - assistant compact summary message references the archive path as before
  - only messages after the resolved boundary survive in the preserved tail, subject to current live-tail rules
- Add `quine-core` integration coverage for the end-to-end compaction flow:
  - valid session-memory state bypasses the generic summarizer path and uses `summary.md`
  - unusable session-memory state still archives and compacts successfully via the current summarizer flow
  - archived tool-result placeholders and transcript archive output remain unchanged in behavior
  - auto-compaction and manual compaction both honor the same selection logic
- Add refresh-coordination coverage:
  - when a refresh is in flight and can be awaited cheaply, compaction consumes the completed snapshot
  - when it cannot be awaited safely, compaction falls back instead of hanging or failing
- Add restore/backward-compatibility coverage if persisted metadata changes:
  - checkpoints written before this feature still restore
  - restored sessions without usable session-memory metadata still compact via the legacy path
- Eventual implementation PR validation commands:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Reviewed the latest QA plan revision in `features/plans/038-session-memory-compaction-qa.md` after it was completed and cross-checked it against `features/038-session-memory-compaction.md`, `docs/design/002-memory-systems-design.md`, `CLAUDE.md`, and the current CLI/harness entrypoints.
- The QA plan now provides an executable and appropriately scoped validation strategy:
  - it stays limited to session-memory-driven compaction in `quine-core`
  - it explicitly preserves the legacy summarizer fallback as a required success path
  - it does not require any shared inter-crate trait changes or new external protocol surface
- The scenario set is concrete and sufficient for this feature’s risk profile:
  - focused unit coverage for source selection and boundary validation
  - integration coverage for valid-session-memory selection, fallback conditions, refresh coordination, and optional restore compatibility
  - a real daemon-backed multi-round `/compact` scenario with exact setup commands, exact user messages, expected outputs, and on-disk artifact inspection
  - explicit parity coverage proving manual and auto-compaction share the same selection logic
- QA expectations the implementation should continue to honor so the plan remains honestly executable:
  - keep the compacted summary message shape compatible with `compaction::compacted_history(...)`, so source selection can be verified via summary body/content rather than a brand-new wrapper contract
  - preserve transcript archive emission before history replacement and keep archive artifacts observable under the existing archive root
  - keep any session-memory compaction bookkeeping additive and restore-safe; if persisted bookkeeping is ultimately unnecessary, the restore-compatibility scenario can be marked not applicable with evidence
  - make refresh coordination bounded and failure-tolerant so fallback remains available without indefinite waiting
  - avoid depending on new operator-facing diagnostics for correctness; internal assertions and stable filesystem artifacts are sufficient
- One QA-doc correction was necessary during review and is now reflected there: the daemon-backed scenario uses `cargo run --bin quine-harness -- start --socket ... --state-dir ...`, which matches the actual CLI surface and makes the scenario directly executable.
- No blocking QA objections remain. After reviewing both docs’ latest revisions, they are aligned and can honestly record `agreed` with no unresolved open questions.
