# 041 Memory Diagnostics and Operator Visibility — Implementation Plan

Short summary: Add structured read-only diagnostics for session-memory and persistent-memory behavior, expose them through additive harness/session inspection surfaces, and support CLI/debug visibility without changing memory semantics.

## Open Questions

- None from implementation planning at this stage. The feature can stay scoped to additive diagnostics and operator visibility on top of the memory slices already planned in Features 037–040, without introducing mutation flows, repair tooling, or shared inter-crate trait changes.

## Agreement Status

agreed — I reviewed the latest paired QA plan revision, and the two docs now align on additive `get_session_context` / `/context` exposure, bounded latest-turn diagnostics, required crate-level coverage, and exact multi-round local-daemon scenarios for both positive and negative memory-diagnostics paths; there are no unresolved open questions.

## Proposed Design

- Scope this work to Feature 5 in `docs/design/002-memory-systems-design.md`: structured, read-only observability for memory behavior that makes the existing and already-planned memory lifecycle debuggable without altering prompt, extraction, refresh, or compaction semantics.
- Keep the implementation additive and narrowly layered:
  - `quine-core` owns the canonical diagnostics data model and records per-turn memory outcomes.
  - `quine-harness` extends the existing session-context inspection snapshot with additive diagnostics fields and continues exposing them through the current `get_session_context` flow.
  - `quine-cli` reuses the existing `/context` and context-debug rendering path to show structured memory diagnostics, ideally with a concise top-level summary plus the raw JSON view already available.
  - `quine-sdk` and shared orchestration traits remain unchanged.
- Build on the memory module layout already established by the design roadmap. This slice should add a dedicated internal diagnostics module under `crates/quine-core/src/memory/`, for example `memory/diagnostics.rs`, rather than spreading diagnostic structs ad hoc across `engine.rs`, `storage.rs`, and CLI snapshot types.
- Treat diagnostics as snapshots of already-decided behavior, never as behavior-driving inputs. The feature must not trigger refresh, extraction, recall, filesystem mutation, or recomputation merely because diagnostics were requested.

### Planned internal diagnostics model

- Add strongly typed, crate-private diagnostics structs in `quine-core` that mirror the design doc’s Feature 5 guidance while staying narrowly scoped to current memory slices:
  - `MemoryTurnDiagnostics`
  - `SessionMemoryDiagnostics`
  - `PersistentMemoryDiagnostics`
  - `PromptMemoryDiagnostics`
  - `MemoryDecisionReason`
  - `MemorySelectionEntryDiagnostics`
  - `MemorySkippedEntryDiagnostics`
- `MemoryTurnDiagnostics` should represent the latest completed turn only in this slice. That keeps payloads bounded and avoids inventing a diagnostic history API before there is a real operator need.
- `SessionMemoryDiagnostics` should capture read-only facts such as:
  - whether session memory was enabled
  - resolved session-memory paths
  - whether a refresh was attempted this turn
  - whether refresh completed, was skipped, or failed best-effort
  - which summary path or compaction boundary source was used
  - last summarized and last compacted boundaries when known
  - reason codes for skipped, stale, missing-file, invalid-boundary, fallback, or in-flight conditions
- `PersistentMemoryDiagnostics` should capture read-only facts such as:
  - whether persistent memory was enabled for the session
  - resolved project-scoped memory root
  - whether prompt-time index injection ran
  - whether targeted recall ran
  - which durable entries were selected
  - which entries were skipped and why, including threshold, stale, duplicate, truncation, and budget reasons
  - whether post-turn extraction ran and its latest create/update/tombstone/ignore outcome summary where already available from prior memory slices
- Use enums for reason/status fields rather than free-form strings so serde snapshots remain stable and QA can assert behavior without scraping logs. Human-readable labels can still be derived in harness/CLI rendering.

### Runtime recording strategy

- Extend `SessionContext` in `crates/quine-core/src/engine.rs` with additive runtime-only memory diagnostics state, e.g. `last_memory_diagnostics: Option<MemoryTurnDiagnostics>`.
- Reuse existing memory lifecycle decision points from prior feature slices rather than recomputing outcomes after the fact:
  - session-memory refresh code records whether it ran, skipped, or failed and which boundary/path metadata was involved
  - compaction code records whether session memory or the legacy summarizer supplied continuity and why fallback happened when applicable
  - prompt-time persistent-memory recall records mode, selected entries, skipped entries, truncation, and stale/duplicate filtering
  - post-turn durable extraction records its latest bounded outcome summary where the earlier slice already made a decision
- Preserve best-effort semantics:
  - if memory maintenance fails but the turn succeeds, diagnostics should reflect the failure without converting it into a user-visible turn error
  - if a memory subsystem is disabled or not initialized, diagnostics should represent that explicitly as a structured status rather than omitting the field unpredictably
- Keep diagnostics bounded:
  - record only the latest turn’s detailed results
  - cap selected/skipped entry lists using the same bounded candidate/result sets already produced by prompt-memory logic
  - avoid embedding raw summary markdown, full entry bodies, or whole indexes in the diagnostics payload

### Inspection surfaces

- Prefer extending the existing `get_session_context` snapshot rather than adding a new protocol method. This keeps operator visibility on the established inspection surface and avoids new client/harness coordination for a read-only feature.
- Add a `memory` or `memory_diagnostics` section to the harness-side `SessionContextSnapshot` assembled in `crates/quine-harness/src/storage.rs`.
- Keep the snapshot additive and backward-compatible for serde consumers by adding optional/defaulted fields rather than changing existing field meanings.
- Surface diagnostics through existing CLI context-debug views:
  - `/context` in chat mode should include the new memory diagnostics fields in its JSON output automatically once the snapshot grows.
  - the TUI context explorer should be extended only as needed to keep the new section navigable and readable; a concise summary block is preferable to a large bespoke memory UI in this slice.
- Do not add mutation or repair commands such as “retry refresh”, “rebuild memory”, or “forget from diagnostics”. Those stay deferred by the feature request.

### Scoped implementation phases

- Phase 1 — canonical diagnostics types in `quine-core`
  - Add `memory/diagnostics.rs` and wire it into the internal memory module tree.
  - Define bounded serde-friendly structs/enums for session-memory, prompt-memory, persistent-memory, and extraction diagnostics.
  - Add focused unit tests for serialization and reason/status stability.
- Phase 2 — runtime capture in `quine-core`
  - Extend `SessionContext` with last-turn diagnostics.
  - Thread diagnostics recording through session-memory refresh, prompt-time persistent-memory injection, persistent-memory extraction, and compaction/fallback paths.
  - Keep all new fields runtime-only unless a narrow restore-safe field is truly needed for context snapshots; prefer latest in-memory snapshot over checkpoint persistence for this feature.
- Phase 3 — harness/session snapshot exposure
  - Extend harness-side snapshot structs in `crates/quine-harness/src/storage.rs` to serialize the diagnostics into `SessionContextSnapshot`.
  - Ensure `get_session_context` continues to work for sessions with no memory activity by returning explicit disabled/empty states rather than failing.
- Phase 4 — CLI/operator visibility
  - Update `crates/quine-cli/src/context_debug.rs` snapshot model to deserialize the additive memory fields.
  - Extend context rendering/TUI navigation only enough to make the new section discoverable and readable.
  - Keep `/context` as the primary operator entry point for this slice.
- Phase 5 — end-to-end validation and regression checks
  - Add unit/integration coverage proving diagnostics remain observational, structured, and additive.
  - Verify daemon-backed `/context` inspection after turns that exercise memory refresh/injection/extraction behavior.

### Dependencies and risks

- Dependency on earlier memory slices is intentional: this feature assumes the implementation shapes from Features 037–040 exist or are implemented compatibly enough that diagnostics can observe them. The diagnostics layer should consume those decisions, not redefine them.
- Main risk: accidental scope creep into a new operator API. Mitigation: keep all visibility on the existing `get_session_context` + `/context` path.
- Main data-model risk: unbounded payload growth from skipped-entry lists or raw file contents. Mitigation: record compact metadata only and cap lists to already-bounded selection results.
- Main architecture risk: introducing a shared trait change to plumb diagnostics. Mitigation: store diagnostics directly in `SessionContext` and expose them through the existing harness snapshot assembly path.
- Main QA risk: unstable human-readable strings. Mitigation: use enums/structured reason fields as the contract and let the CLI derive human labels for display.

## File-by-File Changes

- `crates/quine-core/src/lib.rs`
  - Ensure the internal `memory` module exposes any new crate-private diagnostics submodule needed by the engine and tests.

- `crates/quine-core/src/memory/mod.rs`
  - Re-export the new internal diagnostics types/helpers alongside existing session/persistent memory internals.

- `crates/quine-core/src/memory/diagnostics.rs`
  - Add the canonical diagnostics structs/enums and small helper constructors for bounded status/reason recording.
  - Keep the module crate-private and focused on data modeling rather than filesystem or prompt logic.

- `crates/quine-core/src/engine.rs`
  - Extend `SessionContext` with additive last-turn memory diagnostics state.
  - Record diagnostics at the existing decision points for:
    - session-memory refresh attempts and skips
    - compaction source/fallback outcomes
    - prompt-time persistent-memory injection/recall selection
    - persistent-memory extraction outcome summaries
  - Preserve current behavior exactly; diagnostics should mirror decisions already made.

- `crates/quine-core/src/compaction.rs`
  - If compaction fallback/source details are owned here, add narrow diagnostic return data or helper integration so `engine.rs` can record why session memory was or was not used.
  - Avoid broad API churn; prefer additive return metadata or crate-private helper structs.

- `crates/quine-core/src/persistence.rs`
  - Only add persisted diagnostics fields if implementation proves a restore-safe snapshot requires them. Default expectation is no checkpoint persistence for latest-turn diagnostics because they are operational, not durable state.
  - If any additive persisted field is required, it must be optional/defaulted for backward compatibility.

- `crates/quine-harness/src/storage.rs`
  - Extend `SessionContextSnapshot` with additive `memory_diagnostics` fields and serde serialization.
  - Map `quine-core` diagnostics types into harness snapshot types without changing the existing snapshot contract for unrelated fields.

- `crates/quine-harness/src/server.rs`
  - No new RPC method is expected. Keep using `get_session_context`; only ensure the larger snapshot serializes cleanly through the existing response flow.

- `crates/quine-cli/src/context_debug.rs`
  - Mirror the additive snapshot structs for deserializing memory diagnostics.
  - Keep the raw `/context` JSON path working with minimal churn.

- `crates/quine-cli/src/tui/app.rs`
  - Extend the context explorer model only as needed so memory diagnostics can be navigated without overwhelming the existing UI.

- `crates/quine-cli/src/tui/ui.rs`
  - Add concise rendering helpers for the new diagnostics section if the TUI currently formats `/context` snapshots structurally rather than dumping raw JSON.

- `crates/quine-cli/src/chat.rs`
  - Likely no command-surface change beyond benefiting from the richer `/context` output. Only patch if a concise rendering helper requires it.

- `crates/quine-core/tests/` and/or adjacent module tests
  - Add focused integration coverage for a turn that exercises memory behavior and then inspects the resulting diagnostics snapshot.

## Validation Plan

- Unit tests in `crates/quine-core/src/memory/diagnostics.rs`
  - verify serde roundtrips for the top-level diagnostics payloads
  - verify stable reason/status encoding for skipped, stale, fallback, truncation, and disabled cases
  - verify bounded selected/skipped entry serialization does not require raw file bodies

- Targeted `quine-core` unit/integration tests
  - verify session-memory refresh records `updated`, `skipped`, and `failed-best-effort` outcomes without altering turn success behavior
  - verify prompt-time persistent-memory recall diagnostics record selected and skipped entries with structured reasons
  - verify compaction diagnostics record `session_memory` vs `legacy_summarizer` source and fallback reason when applicable
  - verify persistent-memory extraction diagnostics summarize create/update/tombstone/ignore outcomes if that slice already produces those decisions

- Harness snapshot tests in `crates/quine-harness`
  - verify additive session-context exposure for sessions with no memory activity, active session memory only, active persistent memory only, and both subsystems exercised
  - verify `get_session_context` keeps returning valid snapshots when diagnostics are absent, disabled, or partially populated

- CLI/TUI tests where patterns already exist
  - verify `context_debug` deserializes the new fields and rendering remains readable
  - prefer snapshot/assertion coverage around the concise diagnostics section instead of broad UI rewrites

- Required end-to-end coverage for the eventual implementation PR
  - run a deterministic local daemon session that performs at least one turn causing session-memory refresh or persistent-memory recall/extraction, then inspect `/context` and assert the structured diagnostics fields
  - include at least one fallback/skip scenario so QA can validate why memory was not used, not just the happy path

- Full workspace verification expected for the eventual implementation PR
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Reviewed against the completed QA plan and `docs/design/002-memory-systems-design.md`; the proposed implementation is testable and remains correctly scoped to additive, read-only operator visibility.
- The implementation plan and eventual PR should preserve `get_session_context` / `/context` as the primary inspection contract. QA is explicitly not planning around a new RPC method or a repair-oriented memory command.
- Please ensure the diagnostics schema is explicit enough for the QA scenarios to assert machine-readable outcomes for all four categories already named in this plan:
  - session-memory refresh/update status and reason
  - compaction source/fallback status and reason
  - prompt-time persistent-memory selection/skip status and reason
  - post-turn persistent-memory extraction outcome summary
- The QA plan assumes latest-turn diagnostics are available immediately in the same live session and does not require restart persistence. If implementation details differ, that would reopen agreement because the daemon scenarios are intentionally written around runtime-only visibility.
- The QA plan also assumes bounded payloads: selected/skipped entry metadata is acceptable, but raw summary markdown and full durable-memory file contents should not appear in snapshots.
- With those constraints, QA agrees the implementation plan is complete enough to execute without inventing missing behavior or widening feature scope.
