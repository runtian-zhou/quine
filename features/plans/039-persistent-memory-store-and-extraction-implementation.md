# 039 Persistent Memory Store and Durable Extraction — Implementation Plan

Short summary: Add a project-scoped durable memory store under Quine-managed state, maintain `MEMORY.md` plus one-memory-per-file entries, and implement a conservative post-turn extraction pipeline for create/update/tombstone/ignore decisions without yet changing prompt-time behavior.

## Open Questions

- None from implementation planning at this stage. The feature can stay scoped to additive `quine-core` and `quine-harness` internals, with prompt-time injection, relevant-memory recall, and non-project memory scopes explicitly deferred.

## Agreement Status

agreed — I reviewed the QA plan’s latest revision, it now provides executable coverage including the required concrete multi-round local-daemon scenario for `quine-core`-affecting behavior, and there are no unresolved open questions between the paired docs.

## Proposed Design

- Scope this feature to the Feature 3 slice from `docs/design/002-memory-systems-design.md`: durable project-scoped storage plus best-effort post-turn extraction only.
- Keep the change additive and internal to existing crates:
  - `quine-harness` owns durable state-root resolution, trusted overrides, and project-key derivation.
  - `quine-core` owns persistent-memory models, parsing/rendering, extraction decisions, and deterministic index maintenance.
  - `quine-cli`, `quine-sdk`, and harness protocol surfaces remain unchanged for this slice.
- Preserve the current prompt path in `crates/quine-core/src/engine.rs::build_combined_system_prompt(...)`; this feature must not load `MEMORY.md` or any durable entries into model context.
- Model persistent memory as a project-level store rooted under harness-managed state using the design doc layout:
  - `<state_dir>/memory/projects/<project_key>/MEMORY.md`
  - `<state_dir>/memory/projects/<project_key>/index.json`
  - `<state_dir>/memory/projects/<project_key>/entries/<slug>.md`
  - `<state_dir>/memory/projects/<project_key>/tombstones/<entry_id>.json`
- Use a stable project key derived from the resolved project root so multiple sessions and harness restarts for the same repository share one durable store. Keep any override additive, explicit, and harness-owned.
- Reuse the existing lightweight frontmatter parsing approach in `crates/quine-core/src/skill.rs` as the model for a dedicated persistent-memory parser/renderer, rather than introducing a cross-crate parser abstraction.
- Keep one durable memory per markdown file with inspectable frontmatter and markdown body. The index remains generated output, not the source of truth.

### Planned internal data model

- Add a dedicated internal module, e.g. `crates/quine-core/src/memory/`, with project-scoped types such as:
  - `PersistentMemoryPaths`
  - `PersistentMemoryEntry`
  - `PersistentMemoryRecord`
  - `PersistentMemoryIndex`
  - `PersistentMemoryTombstone`
  - `MemoryExtractionCandidate`
  - `MemoryExtractionDecision`
  - `MemoryExtractionOutcome`
  - `PersistentMemoryExtractionState`
  - `PersistedPersistentMemoryState`
- Keep scope narrow with a `PersistentMemoryScope` that only represents project scope in Feature 3, even if implemented as an enum for future extension.
- Frontmatter should carry enough metadata to rebuild `MEMORY.md` deterministically without reparsing bodies for every update. At minimum the implementation plan should budget for:
  - stable `entry_id`
  - human-readable `title`
  - concise `summary`
  - `keywords`
  - timestamps such as `created_at` / `updated_at`
  - source marker such as `source = explicit|heuristic`
  - status markers needed for tombstone/staleness handling
- Persist only extraction state needed to prevent duplicate work after restart, especially a boundary like `last_extracted_message_index`. Keep any in-flight work markers transient and runtime-only.

### Path resolution and ownership

- Add a harness-owned memory config view in `crates/quine-harness/src/config.rs` for additive persistent-memory settings only. Keep it small and tightly scoped, for example:
  - enable/disable persistent memory
  - trusted root override
  - optional project-key override for tests
  - env-override opt-in for local development/tests
  - enable/disable automatic extraction
  - max extraction decisions per turn
- Resolve the memory root from harness config rather than reading environment variables directly in `quine-core`.
- Base default storage under the existing harness state dir returned by `default_state_dir()` so the memory store remains Quine-managed and separate from per-session checkpoint files.
- Derive the project root from the session working directory using the same upward-walk model already used for `CLAUDE.md` discovery in `engine.rs`; if no repository marker is found, fall back conservatively to the resolved working directory rather than introducing a new git dependency.
- Centralize stable project-key derivation in `quine-harness/src/storage.rs` so every session in the same project maps to the same durable directory.

### Runtime integration

- Extend `SessionContext` in `crates/quine-core/src/engine.rs` with additive persistent-memory runtime state:
  - resolved paths/scope
  - persisted extraction boundary state
  - latest extraction outcome for debugging/tests if useful
  - a transient in-flight guard to avoid overlapping extraction work
- Extend persisted session data in `crates/quine-core/src/persistence.rs` with additive memory-state serialization only if needed for restart safety. Do not persist entry contents or indexes into checkpoints; disk-backed memory files remain the durable source.
- Initialize persistent-memory state when a session is created or restored, similar in spirit to other session-owned runtime state, but without changing cross-crate orchestration traits.
- After a successful turn completes, run extraction on only the newly completed history range after `last_extracted_message_index`.
- Treat extraction as best-effort maintenance:
  - extraction failures must not fail the user-visible turn
  - persistence/index failures should be recorded in runtime outcome state and logged, then normal session flow continues
  - duplicate extraction after restart should be prevented by the persisted boundary

### Extraction policy

- Keep the first release conservative and deterministic.
- Explicit user intent takes precedence over heuristics:
  - explicit “remember this” creates or updates an entry
  - explicit “forget this” tombstones or removes the matching durable fact
  - overlapping heuristic candidates for the same fact are suppressed when an explicit request exists in the turn
- Keep heuristics narrow to durable, non-code-derived facts. Good candidates include:
  - user working preferences that are not already encoded in repo instructions
  - stable project conventions revealed conversationally rather than derivable from source
  - durable external references or follow-up facts the agent should remember across sessions
- Explicitly exclude from heuristics:
  - transient task state
  - current-session worklog details
  - facts directly derivable from repository contents or git history
  - large free-form transcripts
- Bound extraction churn with a per-turn decision cap from config.
- If Quine eventually adds a dedicated memory-management tool or internal write path, extraction for that already-handled range should skip duplicate persistence work. For this feature, the plan should keep a hook for that suppression without requiring the tool to exist yet.

### Persistence and index maintenance

- Treat entry files under `entries/` as the source of truth for live memories.
- Render each entry as a markdown file with frontmatter plus short body text.
- Rebuild `index.json` and `MEMORY.md` deterministically after every batch of applied decisions rather than attempting incremental string edits.
- Keep `MEMORY.md` concise and human-readable:
  - short overview/header explaining it is generated
  - one index item per live durable entry
  - links to `entries/<slug>.md`
  - visible summary/keyword metadata only if it improves inspectability without bloating the file
- Preserve explicit forget semantics with tombstones stored separately from live entries. Tombstones should allow future extraction to avoid immediately recreating recently forgotten facts while keeping forgotten content out of the live index.
- Use atomic write patterns already common to harness persistence where practical so partial writes do not corrupt the index or entry files.

### Suggested implementation phases

- Phase 1: Add harness-owned config and storage-path helpers for project-scoped memory roots and project-key derivation.
- Phase 2: Add `quine-core` persistent-memory module with types, frontmatter parsing/rendering, entry persistence helpers, tombstones, and deterministic index generation.
- Phase 3: Add additive session persistence/runtime state for extraction boundaries and initialization on create/restore.
- Phase 4: Integrate post-turn extraction into `engine.rs` with explicit-intent detection, conservative heuristics, decision application, and best-effort failure handling.
- Phase 5: Add focused unit/integration coverage, plus harness-backed restart/new-session tests proving project-scoped durability.

### Risks and mitigations

- Duplicate extraction across restarts: mitigate by persisting `last_extracted_message_index` and processing only new history.
- Unstable project-key mapping: mitigate by centralizing path normalization and project-key derivation in harness storage helpers with focused tests.
- Over-eager heuristic memory creation: mitigate by keeping heuristics intentionally narrow, capping decisions per turn, and preferring ignore over create when uncertain.
- Index drift from entry files: mitigate by always rebuilding `MEMORY.md` and `index.json` from record state after each applied batch.
- Trait/interface churn: mitigate by keeping all changes behind existing crate boundaries and avoiding shared inter-crate trait contract changes.

## File-by-File Changes

- `crates/quine-harness/src/config.rs`
  - Add additive persistent-memory config structures and default resolution helpers for trusted root override, project-key override, feature enablement, and bounded extraction settings.
  - Add path-resolution tests alongside existing `default_state_dir()` coverage.
- `crates/quine-harness/src/storage.rs`
  - Add helpers to normalize project roots, derive a stable `project_key`, and resolve `<state_dir>/memory/projects/<project_key>/...` paths.
  - Add tests that prove the same project maps to the same durable store across sessions/restarts and that overrides remain scoped to durable memory only.
- `crates/quine-core/src/lib.rs`
  - Re-export any new internal module entry points needed by the harness/core boundary while keeping concrete types crate-private where possible.
- `crates/quine-core/src/memory/mod.rs`
  - New internal module root for persistent-memory functionality.
  - Define small submodule boundaries such as `persistent.rs`, `frontmatter.rs`, or `extract.rs` only if that improves maintainability; avoid over-fragmentation.
- `crates/quine-core/src/memory/persistent.rs` (or equivalent new file)
  - Implement path-bound persistent-memory models, entry/index/tombstone load+write helpers, deterministic `MEMORY.md` rendering, and decision application.
- `crates/quine-core/src/memory/frontmatter.rs` (optional)
  - Implement dedicated parsing/rendering for memory entry frontmatter, likely following the simple parser style already used in `skill.rs`.
- `crates/quine-core/src/memory/extract.rs` (optional)
  - Implement explicit-intent detection, conservative heuristic candidate detection, decision resolution, and extraction outcome reporting.
- `crates/quine-core/src/persistence.rs`
  - Add additive persisted session fields for persistent-memory extraction state if restart-safe duplication prevention requires it.
  - Add serde round-trip tests for new state only.
- `crates/quine-core/src/engine.rs`
  - Extend `SessionContext` with persistent-memory runtime state.
  - Initialize state on session create/restore.
  - Hook best-effort extraction after successful turn completion without changing prompt construction.
  - Ensure extraction respects persisted boundaries and does not interfere with existing tool/session flow.
- `crates/quine-core/tests/` integration tests (new or existing file)
  - Add end-to-end tests for explicit remember/forget flows, deterministic index maintenance, restart safety, and new-session reuse of the same project store.
- `crates/quine-harness/tests/` integration tests if needed
  - Add harness-level restart/new-session durability tests when `LocalHarness` and `StorageManager` are the easiest way to exercise real state-dir behavior.

## Validation Plan

- Unit tests in `quine-harness` for path resolution:
  - default memory root under harness-managed state
  - trusted override precedence
  - project-key override behavior for tests
  - stable project-key derivation for equivalent project roots
- Unit tests in `quine-core` for frontmatter and persistence helpers:
  - parse valid memory entry frontmatter
  - reject malformed frontmatter cleanly
  - render/parse round-trip for entry files
  - one-memory-per-file write/load behavior
  - tombstone write/load behavior
- Unit tests in `quine-core` for index maintenance:
  - deterministic `MEMORY.md` generation from records
  - deterministic `index.json` generation
  - tombstoned entries excluded from live index
  - repeated rebuilds produce stable output ordering
- Unit tests in `quine-core` for extraction logic:
  - explicit remember request produces create/update decision
  - explicit forget request produces tombstone/delete decision
  - heuristic candidates are ignored when facts appear code-derived or transient
  - explicit requests suppress overlapping heuristic actions
  - per-turn decision cap is enforced
- Integration tests for runtime behavior:
  - successful turn triggers extraction only for newly completed message range
  - extraction failures do not fail the turn
  - persistent memory survives harness restart when using the same state dir
  - a new session in the same project sees the same durable store on disk
  - a different project maps to a different durable store
- Full workspace validation expected for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- QA reviewed the latest implementation plan and agrees with the current scope boundaries:
  - no prompt-time `MEMORY.md` injection or targeted recall in this slice
  - project scope only; no team or agent durable memory variants
  - no shared inter-crate trait contract changes
- Please keep restart-safe duplicate suppression explicit in implementation artifacts. The QA plan requires at least one persisted extraction-boundary test so the same remembered fact is not recreated after restart.
- Because the required daemon-backed test must prove same-project reuse and different-project isolation, the implementation should preserve a testable way to create sessions with distinct `working_directory` values through the harness path. The current `SessionConfig` already carries `working_directory`; please keep the feature additive and avoid inventing a memory-specific session-creation interface.
- The required local-daemon test should use a deterministic in-process provider, not a network model, so that final response text is exact and stable. An echo-style provider is sufficient and already exists in harness test patterns.
- Please preserve best-effort extraction semantics in runtime integration tests: a persistence/index write failure must not turn the visible user turn into an error, and QA will look for explicit coverage of that negative path.
- The manual smoke scenario in the QA plan now matches the current CLI surface: start the daemon with `cargo run --bin quine-harness -- start ...`, then run `cargo run --bin quine -- run <message> --socket ... --json` from the intended project directory because the one-shot CLI does not currently expose a dedicated `working_directory` flag.
- From QA’s perspective the paired implementation plan is agreed with no open questions.
