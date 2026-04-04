---
status: done
---

# Session Memory Foundation and Summary Lifecycle

## Overview

Implement the first Quine memory-system slice by laying the architectural groundwork for memory-related session state and introducing a per-session `session-memory/summary.md` lifecycle.

This feature should remain intentionally narrow:

- add internal memory module seams in `quine-core`
- add per-session memory bookkeeping to the runtime and persisted metadata where needed
- create and maintain a structured session summary file for each session
- update that summary asynchronously after completed turns
- keep the feature independent from compaction changes and persistent cross-session memory recall

The goal is to give Quine a stable session-memory foundation that preserves continuity metadata on disk and prepares the codebase for later compaction and durable-memory features.

## Requirements

### 1. Add internal memory module groundwork in `quine-core`

Introduce an internal memory module layout under `crates/quine-core/src/` that can own session-memory logic now and persistent-memory logic later.

The first version should keep APIs additive and internal where possible.

At minimum, the groundwork should make room for:

- session-memory logic
- persistent-memory logic
- memory diagnostics or bookkeeping

This feature must not require changes to the shared `Tool`, `Agent`, `Dispatcher`, `HarnessService`, or `LlmProvider` trait contracts.

### 2. Add per-session memory state bookkeeping

Extend session runtime state so the core can track session-memory-related metadata such as:

- whether session memory is enabled
- whether a summary update is in flight
- the last summarized message boundary or equivalent marker
- the current session-memory path
- timestamps or counters useful for refresh decisions

Only durable restore state should be persisted. Raw summary contents should stay on disk rather than being duplicated into checkpoints.

### 3. Define a Quine-owned session-memory storage layout

Add a stable on-disk storage layout for session memory associated with a specific session.

The initial design should include:

- a session-memory directory under Quine-managed local state for the session
- a `summary.md` file
- any small sidecar metadata file or embedded footer needed to record machine-readable summary boundary information

The storage layout should be easy to inspect manually and robust to missing files.

### 4. Initialize `summary.md` from a structured template

When session memory is first needed for a session, create `summary.md` from a Quine-owned template.

The template should preserve a consistent structure so later logic can update and consume it deterministically. It should reflect the design doc’s intended sections such as current state, files/functions, workflow, errors/corrections, and worklog, but the implementation may refine the exact section list if needed for Quine’s architecture.

### 5. Update session memory after completed turns

Add a background maintenance path that runs after completed turns and refreshes `summary.md` asynchronously.

The first implementation may use deterministic Quine-owned summarization logic, or a tightly controlled internal LLM call orchestrated by the core, but it should not spin up a separate user-visible autonomous agent flow.

The update path must:

- snapshot the relevant transcript state safely
- decide when a refresh is needed
- serialize writes per session to avoid races
- tolerate missing files or disabled state without breaking the main chat flow

### 6. Preserve restore/resume behavior

If the harness persists and restores sessions, the restored session should retain enough metadata to continue session-memory maintenance correctly.

The feature should define clearly which memory metadata belongs in checkpoints and which remains only on disk.

### 7. Add focused tests

Add tests for:

- summary template initialization
- summary refresh decision logic
- boundary metadata parsing/serialization
- per-session write serialization or equivalent race prevention
- checkpoint/restore handling for persisted session-memory metadata
- end-to-end creation and update of `summary.md` across one or more turns

## Planned Data Structures and Internal APIs

This feature should follow the Phase 0 and Feature 1 entries in the `## Planned Data Structure and API Changes` section of `docs/design/002-memory-systems-design.md`.

Planned concrete structures for this feature:

- `SessionMemoryPaths`
  - `directory: PathBuf`
  - `summary_path: PathBuf`
  - `metadata_path: PathBuf`
- `SessionMemoryState`
  - `enabled: bool`
  - `paths: SessionMemoryPaths`
  - `refresh_in_flight: bool`
  - `last_summarized_message_index: Option<usize>`
  - `last_refresh_at: Option<DateTime<Utc>>`
  - `template_version: u32`
- `SessionSummaryMetadata`
  - `last_summarized_message_index: usize`
  - `updated_at: DateTime<Utc>`
  - `template_version: u32`
- `SessionSummaryDocument`
  - structured sections for `Current State`, `Task Specification`, `Files and Functions`, `Workflow`, `Errors & Corrections`, `Codebase and System Documentation`, `Learnings`, `Key Results`, and `Worklog`
- `SessionSummaryUpdate`
  - `from_message_index: usize`
  - `to_message_index: usize`
  - `document: SessionSummaryDocument`
  - `metadata: SessionSummaryMetadata`
- `PersistedMemoryState`
  - `session_memory: PersistedSessionMemoryState`
- `PersistedSessionMemoryState`
  - `enabled: bool`
  - `last_summarized_message_index: Option<usize>`
  - `template_version: u32`

Planned `SessionContext` additions:

- `session_memory: SessionMemoryState`
- `memory_diagnostics: MemoryDiagnostics` or a narrower session-memory-only diagnostic field if the final implementation keeps persistent-memory diagnostics deferred

Planned internal helper APIs:

- `session_memory_paths(state_root: &Path, session_id: SessionId) -> SessionMemoryPaths`
- `restore_memory_state(persisted: Option<&PersistedMemoryState>) -> SessionMemoryState`
- `snapshot_memory_state(state: &SessionMemoryState) -> PersistedMemoryState`
- `initialize_summary_if_missing(paths: &SessionMemoryPaths) -> anyhow::Result<()>`
- `load_summary_metadata(path: &Path) -> anyhow::Result<SessionSummaryMetadata>`
- `store_summary_metadata(path: &Path, metadata: &SessionSummaryMetadata) -> anyhow::Result<()>`
- `should_refresh_summary(state: &SessionMemoryState, history: &[Message]) -> bool`
- `build_summary_update(...) -> SessionSummaryUpdate`
- `refresh_summary_from_history(...) -> anyhow::Result<()>`
- additive engine methods such as:
  - `maybe_schedule_session_memory_refresh(&mut self) -> bool`
  - `mark_session_memory_refresh_started(&mut self)`
  - `mark_session_memory_refresh_finished(&mut self, ...)`

Constraints for these structures and APIs:

- all of them should remain crate-private to `quine-core` unless testing or inspection requires additive exposure
- `PersistedSession` should gain only an optional/additively defaulted `memory_state` field
- no raw summary markdown should be stored in checkpoints
- no shared inter-crate trait changes are allowed for this feature

## Acceptance Criteria

- `cargo build` passes.
- `cargo test` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo fmt --all -- --check` passes.
- Quine can create a per-session `summary.md` file in a stable session-memory location.
- After completed turns, Quine updates `summary.md` asynchronously without interrupting normal response generation.
- Missing or disabled session-memory state does not break the conversation flow.
- Restored sessions retain enough metadata to continue session-memory maintenance correctly.
- No shared inter-crate trait contract is modified to make this feature work.

## Non-Goals (Deferred)

- Using session memory to drive compaction
- Persistent cross-session durable memory files and `MEMORY.md`
- Prompt-time memory injection or relevant-memory recall
- Team-scoped or agent-scoped memory variants
- Rich CLI UX for editing or inspecting memory beyond basic diagnostics
