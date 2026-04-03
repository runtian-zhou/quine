# 037 Session Memory Foundation and Summary Lifecycle — Implementation Plan

Short summary: Add the first Quine memory-system slice by introducing internal `quine-core` memory module seams, per-session memory bookkeeping, a Quine-owned `session-memory/summary.md` storage layout and template, and an asynchronous post-turn summary refresh path that survives checkpoint/restore, without yet using session memory for compaction or adding persistent cross-session recall.

## Open Questions

- None from implementation planning. The implementation should use deterministic Quine-owned summarization in this feature slice rather than a new internal LLM summarizer path, which keeps scope inside Phase 0 groundwork + Feature 1 and avoids introducing extra provider orchestration and test instability.

## Agreement Status

agreed — reviewed the latest QA plan revision and it now provides executable unit, integration, restore, missing-file-recovery, and daemon-backed scenarios aligned with this implementation plan; both docs agree on scope and there are no unresolved open questions.

## Proposed Design

- This plan implements the Phase 0 and Feature 1 data/API model documented in `docs/design/002-memory-systems-design.md`.
- The implementation should keep the concrete names aligned with that design section unless small naming changes materially improve code clarity.
- The initial code should include the agreed internal structs:
  - `SessionMemoryPaths`
  - `SessionMemoryState`
  - `SessionSummaryMetadata`
  - `SessionSummaryDocument`
  - `SessionSummaryUpdate`
  - `PersistedMemoryState`
  - `PersistedSessionMemoryState`
- `SessionContext` should gain additive `session_memory` state and any narrow diagnostics field needed for best-effort refresh bookkeeping.
- `PersistedSession` should gain an optional/additively defaulted `memory_state` field used only for restore-relevant metadata.
- Summary file contents remain on disk under the harness state root and are never embedded into checkpoints.
- Internal helper APIs should follow the design doc’s Phase 0 / Feature 1 plan, specifically helpers for path resolution, restore/snapshot of memory state, summary initialization, metadata load/store, refresh eligibility, summary building, and async refresh execution.
- Add a new internal `memory` module tree under `crates/quine-core/src/` with additive, crate-owned seams only:
  - `memory/mod.rs` re-exports internal helpers.
  - `memory/session.rs` owns session-memory runtime metadata, path resolution, refresh policy, boundary bookkeeping, and write serialization helpers.
  - `memory/template.rs` owns the initial `summary.md` template renderer.
  - `memory/summary.rs` owns deterministic transcript-to-summary refresh logic and sidecar metadata parsing/serialization.
  - Reserve `memory/persistent.rs` or a placeholder module comment for future persistent-memory work so the layout clearly separates session memory from deferred durable memory features.
- Keep all new memory APIs internal to `quine-core`; do not modify shared `Tool`, `Agent`, `Dispatcher`, `HarnessService`, or `LlmProvider` traits.
- Extend `engine.rs::SessionContext` with additive session-memory state rather than introducing a new cross-crate interface. The runtime should track:
  - whether session memory is enabled for the session
  - the resolved session-memory directory and `summary.md` path
  - whether a refresh task is currently in flight
  - the last summarized transcript boundary marker
  - timestamps/counters needed to skip redundant refreshes
- Persist only durable restore metadata in checkpoints by extending `PersistedSession` or `PersistedSessionConfig` with a small serializable memory-state struct. Do not persist summary file contents into checkpoints.
- Store session-memory artifacts underneath the existing harness-owned state root, using a Quine-managed per-session layout parallel to other session artifacts:
  - `<state_dir>/sessions/<session_id>/session-memory/summary.md`
  - `<state_dir>/sessions/<session_id>/session-memory/summary.meta.json` (preferred for machine-readable boundary/state data)
- Resolve the session-memory path from the core’s existing `archive_root` / harness state root rather than from working-directory-relative state. This matches current ownership of compaction archives and checkpoints and avoids mixing generated continuity artifacts into the user repo.
- Initialize `summary.md` lazily on first eligible post-turn maintenance pass instead of at session creation. That keeps startup cheap and ensures the file exists only when memory maintenance is actually active.
- Trigger summary maintenance after completed turns, on a best-effort background path that:
  - snapshots the session transcript and memory metadata after the normal response is complete
  - returns control to the main chat flow immediately
  - serializes writes per session with a session-local guard so only one refresh runs at a time
  - skips work if disabled, already in flight, or no new transcript boundary has been crossed
  - tolerates missing directories/files by recreating them from template/metadata defaults
  - logs failures as session errors or debug logs without failing the turn itself
- Use a deterministic summarizer for Feature 1:
  - derive a structured summary from transcript messages and tool results already held in `session.history`
  - update fixed sections in `summary.md` from parsed transcript facts
  - append/update a concise worklog based on newly summarized messages
  - store the last summarized message index/count in `summary.meta.json`
- Keep compaction behavior unchanged in this feature. Session memory is generated and maintained now, but compaction does not consume it yet.
- Restore behavior:
  - on session restore, reconstruct runtime session-memory state from persisted metadata plus the stable on-disk path
  - if `summary.md` or the sidecar is missing, treat it as recoverable and recreate on next eligible refresh
  - if checkpoint metadata is absent (older checkpoints), default to disabled/not-yet-initialized state for backward compatibility
- Add focused tests in the owning modules and `crates/quine-core/tests/` for template initialization, refresh decisions, sidecar parsing, concurrency guards, restore behavior, and end-to-end multi-turn summary creation/update.

## File-by-File Changes

- `crates/quine-core/src/lib.rs`
  - Add `mod memory;` and keep it crate-private unless a test/re-export need proves otherwise.
- `crates/quine-core/src/memory/mod.rs`
  - Introduce the internal memory module entrypoint and shared internal types.
- `crates/quine-core/src/memory/session.rs`
  - Define the concrete runtime structs from the design doc:
    - `SessionMemoryPaths`
    - `SessionMemoryState`
  - Define refresh-decision helpers, transcript boundary representation, restore/snapshot helpers, and per-session write-serialization primitives.
  - Keep this focused on runtime bookkeeping and filesystem path ownership.
- `crates/quine-core/src/memory/template.rs`
  - Define `SessionSummaryDocument` rendering into the canonical `summary.md` template.
  - Template should include stable headings aligned with the roadmap: `Current State`, `Task Specification`, `Files and Functions`, `Workflow`, `Errors & Corrections`, `Codebase and System Documentation`, `Learnings`, `Key Results`, and `Worklog`.
- `crates/quine-core/src/memory/summary.rs`
  - Define the concrete metadata/update structs from the design doc:
    - `SessionSummaryMetadata`
    - `SessionSummaryUpdate`
  - Implement deterministic summary refresh logic.
  - Implement sidecar metadata serde roundtrips and safe create/update helpers.
- `crates/quine-core/src/engine.rs`
  - Extend `SessionContext` with the planned `session_memory` field and any narrow diagnostic bookkeeping needed for best-effort refresh tracking.
  - Initialize session-memory state from `session_memory_paths(...)` plus `restore_memory_state(...)` when creating or restoring a session.
  - Snapshot the runtime state back into `PersistedMemoryState` during persistence.
  - After successful turn completion, enqueue or spawn a best-effort async session-memory refresh using a cloned transcript snapshot.
  - Ensure refresh scheduling never blocks `TextComplete`, `TurnComplete`, or checkpoint emission for the user-visible turn.
  - Ensure only one refresh is active per session and that completion/failure clears the in-flight marker.
- `crates/quine-core/src/persistence.rs`
  - Add the concrete persisted structs from the design doc:
    - `PersistedMemoryState`
    - `PersistedSessionMemoryState`
  - Thread an optional/additively defaulted `memory_state` field into `PersistedSession` with backward-compatible defaults.
  - Add unit tests for serialization and restore defaults.
- `crates/quine-harness/src/storage.rs`
  - Update session-context debug snapshots if helpful to expose memory metadata/path for diagnostics.
  - Keep this additive and low-risk; do not make CLI UX a dependency for the feature.
- `crates/quine-harness/src/config.rs`
  - No required behavior change expected; implementation should continue using the existing `state_dir` as the root for session-memory artifacts.
  - If needed, add documentation comments clarifying that session-managed artifacts include compactions and session memory.
- `crates/quine-core/tests/session_memory_foundation.rs` (or similar new integration test)
  - Add end-to-end tests covering summary file creation, refresh across turns, and checkpoint/restore continuity.

## Implementation Steps

- Define the internal memory module structure and core data types first, without wiring refresh execution yet.
- Add persisted session-memory metadata with serde defaults so older checkpoints still load.
- Extend `SessionContext` to carry runtime memory state derived from the harness state root plus session id.
- Implement path helpers and lazy initialization of `session-memory/summary.md` plus `summary.meta.json`.
- Implement refresh-decision logic based on transcript length / last summarized boundary and in-flight state.
- Implement deterministic summarization that rewrites the structured summary from a transcript snapshot.
- Wire post-turn background refresh scheduling into the successful-turn path in `engine.rs`.
- Add unit and integration tests, then tighten any diagnostics exposure needed for QA observability.

## Risks and Mitigations

- Background-task races could corrupt `summary.md`.
  - Mitigation: keep a per-session in-flight guard in runtime state and use atomic rewrite helpers for file output.
- Checkpoint restore could duplicate ephemeral state.
  - Mitigation: persist only small boundary/path-enabled metadata; reconstruct runtime guards and task handles on restore.
- Deterministic summarization may miss details compared with an LLM summary.
  - Mitigation: scope Feature 1 to stable groundwork and continuity metadata, not high-fidelity semantic summarization; keep module seams ready for future LLM-backed refinement.
- Path ownership may drift from existing state layout.
  - Mitigation: derive paths from the harness-owned state root already passed into `run_core_loop_with_compaction` and avoid new global configuration or working-tree writes.
- Older checkpoints may not contain new memory fields.
  - Mitigation: use `#[serde(default)]` on new persisted structs and treat missing metadata as “not yet summarized.”

## Validation Plan

- Unit tests in `crates/quine-core/src/memory/template.rs`
  - Verify `SessionSummaryDocument` rendering contains the required stable headings.
- Unit tests in `crates/quine-core/src/memory/session.rs`
  - Verify `SessionMemoryState` initialization, restore defaults, refresh-decision logic for disabled state, no-new-boundary cases, and refresh-needed cases.
  - Verify per-session write serialization state transitions.
- Unit tests in `crates/quine-core/src/memory/summary.rs`
  - Verify `SessionSummaryMetadata` serde roundtrip.
  - Verify `SessionSummaryUpdate` advances boundaries correctly.
  - Verify missing-file recovery and atomic rewrite behavior against temp directories.
- Unit tests in `crates/quine-core/src/persistence.rs`
  - Verify checkpoint serialization/deserialization of `PersistedMemoryState` / `PersistedSessionMemoryState` and backward-compatible defaults.
- Integration test in `crates/quine-core/tests/session_memory_foundation.rs`
  - Start a core loop with a mock provider.
  - Send at least two user turns.
  - Assert `summary.md` is created under the stable state-root session path and updated after later turns.
  - Assert `summary.meta.json` round-trips into `SessionSummaryMetadata`.
  - Assert the sidecar boundary advances through a new `SessionSummaryUpdate`.
- Integration restore test in `crates/quine-core/tests/session_memory_foundation.rs`
  - Snapshot a session after at least one refresh.
  - Restore from checkpoint.
  - Send another turn.
  - Assert refresh resumes correctly instead of restarting from zero or failing on missing runtime guards.
- Run daemon-backed validation to confirm the on-disk API contract for this feature exists exactly as planned:
  - `<state_dir>/sessions/<session_id>/session-memory/summary.md`
  - `<state_dir>/sessions/<session_id>/session-memory/summary.meta.json`
- Workspace verification for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Latest QA doc review status: reviewed the completed QA plan and the latest implementation plan revision; agreement is now reached with no unresolved open questions.
- The implementation plan is aligned with QA on the feature boundaries:
  - internal-only `quine-core` memory seams
  - harness-state-root storage at `<state_dir>/sessions/<session_id>/session-memory/summary.md` plus `summary.meta.json`
  - deterministic in-core summarization for Feature 1
  - async best-effort post-turn refresh that does not block the user-visible turn
  - restore-safe persisted metadata without persisting raw summary contents into checkpoints
- Required QA execution coverage for the eventual implementation PR:
  - targeted `quine-core` unit tests for template headings, refresh decisions, sidecar roundtrips, and write serialization
  - `quine-core` integration tests for first-turn creation, multi-turn boundary advancement, restore/resume continuation, and missing-file recovery
  - at least one exact multi-round daemon-backed chat scenario that inspects the generated `session-memory` files under a temporary harness state root
  - evidence from logs or equivalent observation that `turn_complete` remains healthy while summary maintenance happens asynchronously/best-effort afterward
- One implementation detail to preserve for QA executability: ensure the resulting session-memory file paths and persisted boundary markers are discoverable either directly on disk or via existing checkpoint/log inspection so the daemon scenarios can verify them without guessing.
- No further QA-side changes are required before implementation work begins.
