# 042 Advanced Memory Scopes and Policy Controls — Implementation Plan

Short summary: Extend Quine durable memory beyond the existing project-scoped behavior to optional team and agent scopes, add additive policy/config controls for scope-specific read and write authorization, and define deterministic lookup precedence and conflict resolution without widening the feature into new UI flows or shared trait changes.

## Open Questions

- None. The paired implementation and QA docs now align on additive advanced scope and policy behavior built on top of the earlier memory slices, with no unresolved scope, API, or validation questions.

## Agreement Status

agreed — I reviewed the paired QA plan’s latest revision after it was tightened with shared daemon setup and concrete executable daemon scenarios, including exact commands, exact round-by-round messages, and exact expected outputs, status, tool activity, and inspection behavior. The two docs now agree on scope, validation depth, executable QA expectations, and non-goals, with no unresolved open questions.

## Proposed Design

- Scope this work to Feature 6 in `docs/design/002-memory-systems-design.md`, assuming the earlier memory slices already exist:
  - project-scoped durable memory, extraction, prompt-time recall, and diagnostics remain the baseline
  - this feature adds optional team and agent scopes plus explicit policy enforcement on top of those existing behaviors
  - it does not introduce new memory-management commands, conflict-resolution UI, or distributed/shared synchronization
- Keep the implementation additive and narrowly layered:
  - `quine-core` owns scope models, scope/path resolution, policy evaluation, permission helpers, lookup ordering, conflict resolution, and runtime scoped-memory state
  - `quine-harness` owns trusted config parsing, state-root and override resolution, stable key derivation inputs, and any additive session-start/session-context plumbing needed to pass policy snapshots into core
  - `quine-cli` should only receive minimal additive session-start wiring if custom-agent metadata or diagnostics already have an existing path; avoid broad new user-facing command surfaces in this slice
  - `quine-sdk` and shared orchestration traits remain unchanged
- Extend the persistent-memory model rather than inventing a parallel memory system:
  - reuse the existing durable entry/index formats from the earlier persistent-memory features
  - add a stable `PersistentMemoryScope` discriminator so records, indexes, extraction, prompt-time recall, and diagnostics can all reason about project, team, and agent scopes consistently
  - keep scope keys explicit in typed structs rather than encoding meaning in free-form strings or filesystem paths alone
- Follow the design doc’s planned storage layout under harness-managed state:
  - `<state_dir>/memory/projects/<project_key>/...`
  - `<state_dir>/memory/teams/<team_key>/...`
  - `<state_dir>/memory/agents/<project_key>/<agent_key>/...`
  - each scope keeps its own `MEMORY.md`, `index.json`, and `entries/` tree
- Introduce a dedicated internal scope/policy layer in `crates/quine-core/src/memory/` so prompt-building, extraction, and diagnostics consume pre-resolved state instead of each subsystem re-deriving rules ad hoc.

### Planned internal model

- Extend or refine the internal persistent-memory types to support Feature 6 explicitly:
  - `PersistentMemoryScope`
  - `ProjectMemoryScope`
  - `AgentMemoryScope`
  - `TeamMemoryScope`
  - `MemoryScopeRef`
  - `ScopedMemoryPaths`
  - `ScopedMemoryResolution`
  - `ScopedMemorySelection`
  - `ScopedMemoryLookupOrder`
  - `MemoryConflictResolution`
  - `PersistentMemoryScopeState`
  - `ResolvedMemoryPolicies`
  - `ScopedPersistentMemoryState`
- Add additive policy/config types with a clean separation of concerns:
  - `MemoryFeatureFlags` for coarse enablement of session memory, persistent memory, targeted recall, and advanced scopes
  - `MemoryReadPolicy` for which scopes can be read and whether cross-scope recall is allowed
  - `MemoryWritePolicy` for which scopes can be written and under what trust or explicit-intent constraints
  - `MemoryScopePolicy` for default write scope, lookup order, and conflict rule selection
  - `MemoryAccessPolicy` / `MemoryPolicyConfig` as validated config snapshots coming from `quine-harness`
  - `MemoryPermissionContext` for per-session/per-turn authorization inputs such as trusted workspace, active agent key, active team key, and explicit remember/forget intent
- Keep persisted checkpoint growth conservative:
  - runtime scope resolution should be rebuilt on restore from config and working-directory context
  - persisted memory state only needs restore-relevant fields such as active scope defaults, readable scopes, writable scope, and extraction boundaries
  - do not persist full entry contents, indexes, or bulky diagnostics into checkpoints

### Deterministic precedence and conflict rules

- Keep first-release lookup precedence simple, explicit, and testable:
  - project-only when advanced scopes are disabled
  - project plus agent when agent scope is enabled and an active agent key is available
  - project plus team when team scope is enabled and a team key is configured
  - project plus agent plus team only when cross-scope recall is explicitly enabled in policy
- Default write behavior should remain conservative:
  - project scope remains the default write target unless policy chooses a narrower default scope
  - team and agent writes must pass explicit scope authorization checks
  - denied writes must fail safe and must not silently fall back to a broader scope without an explicit policy rule
- Conflict resolution should be deterministic and diagnostic-friendly:
  - implement a dedicated helper that resolves equivalent facts across scopes according to configured `MemoryConflictResolution`
  - support the design doc’s planned strategies such as `PreferNarrowerScope`, `PreferBroaderScope`, `PreferMostRecentlyUpdated`, and `ErrorOnConflictingWrites`
  - keep lower-priority records intact unless an explicit remember/update/forget decision targets that scope directly
  - ensure the winning scope and reason are available to existing diagnostics plumbing rather than hiding the resolution decision

### Permission and trust integration

- Treat memory scope authorization as an internal policy layer aligned with Quine’s existing trust/permission model rather than a new global permission framework.
- Resolve a `MemoryPermissionContext` before prompt recall or extraction runs, using facts already available at runtime:
  - whether the workspace is trusted according to the existing filesystem trust model
  - whether the current memory decision comes from explicit remember/forget intent versus heuristic extraction
  - whether an active agent key or team key exists for the session
- Separate read checks from write checks:
  - recall should only read policy-authorized scopes in configured order
  - extraction should write to exactly one authorized scope for each decision
  - if writes are denied, preserve best-effort turn semantics and surface the denial through diagnostics rather than turning the whole user turn into an error unless the existing memory subsystem already treats that path as a hard failure

### Runtime integration boundaries

- Extend `SessionContext` in `crates/quine-core/src/engine.rs` with additive scoped-memory runtime state, likely alongside the earlier persistent-memory state:
  - resolved scope metadata for the session
  - active policy snapshot
  - readable scopes in deterministic order
  - writable scope or explicit no-write state
  - per-session agent/team scope identifiers when present
- Ensure scope resolution happens once during session initialization or restore, then gets reused by prompt-time recall, extraction, and diagnostics rather than recalculated differently in each path.
- Prefer internal helper-based integration points instead of trait changes:
  - prompt-building paths should consume resolved readable scopes and conflict rules
  - extraction paths should consume resolved writable scope and permission checks
  - diagnostics should report the same resolved state and denial/conflict reasons already computed by the memory layer
- Avoid a broad UI expansion:
  - if custom-agent sessions need an `agent_key`, thread it through an additive session config/startup field or reuse existing skill/session metadata pathways
  - do not add a new standalone memory-control command surface in this feature

### Scoped implementation phases

- Phase 1 — scope and policy groundwork in `quine-core`
  - Add internal scope, lookup-order, conflict-resolution, and policy types under `crates/quine-core/src/memory/`.
  - Add serde-stable unit coverage for those enums/structs where persisted snapshots or diagnostics depend on them.
  - Define helper functions for scope/path resolution, read/write authorization, and conflict resolution.
- Phase 2 — trusted config and resolution inputs in `quine-harness`
  - Add additive harness config fields for advanced scope enablement, roots, keys, lookup order, and policy requirements.
  - Resolve validated `MemoryPolicyConfig` snapshots in harness-owned code rather than letting `quine-core` read raw env/config directly.
  - Add or preserve a minimal path to provide `agent_key` and `team_key` context to session creation when configured.
- Phase 3 — runtime state wiring and restore behavior
  - Extend `SessionContext` and persisted memory/session state with restore-relevant scoped-memory fields only.
  - Resolve scoped memory state during session startup and restore.
  - Keep restore behavior backward-compatible with older checkpoints by defaulting new optional fields.
- Phase 4 — prompt-time recall and extraction enforcement
  - Update the existing prompt-time persistent recall helpers to read from all authorized scopes in deterministic order.
  - Update extraction helpers to select exactly one authorized writable scope per decision.
  - Apply conflict resolution and de-duplication before ranking/injecting prompt memories when multiple scopes expose overlapping facts.
- Phase 5 — diagnostics and inspection alignment
  - Ensure Feature 5 diagnostics surfaces report resolved readable scopes, writable scope, denied-scope reasons, and conflict winners using the already computed runtime state.
  - Keep diagnostics additive and bounded; do not dump full file contents or indexes.

### Dependencies and sequencing

- This feature depends on the earlier memory slices already existing, especially:
  - persistent project-scoped storage and extraction from Feature 039
  - prompt-time persistent recall from Feature 040
  - diagnostics visibility from Feature 041
- Implementation should preserve compatibility when those earlier systems are disabled or partially configured:
  - advanced scopes disabled should reduce cleanly to project-only behavior
  - relevant-memory disabled should bypass multi-scope recall even if scope resolution exists
  - persistent-memory disabled should bypass durable scope handling entirely
- Recommended implementation order inside the eventual PR:
  1. add types and pure helpers
  2. add harness config and validated snapshots
  3. wire runtime state and restore support
  4. integrate recall and extraction against resolved scoped state
  5. update diagnostics exposure and end-to-end tests

### Risks and containment

- **Scope creep risk:** Advanced scopes can easily turn into a broad UX and admin surface. Contain this by limiting the feature to internal behavior plus minimal startup/config plumbing and diagnostics already justified by Feature 041.
- **Trait-change risk:** Session metadata for agent/team scope could tempt changes to shared interfaces. Contain this by using additive config/session fields and internal helper wiring rather than modifying shared `Tool`, `Agent`, `Dispatcher`, `HarnessService`, or `LlmProvider` traits.
- **Ambiguous precedence risk:** Multiple readable scopes can create hard-to-debug behavior. Contain this by making lookup order explicit in typed config and by reusing one conflict-resolution helper everywhere.
- **Permission drift risk:** Memory policy logic could diverge from filesystem trust expectations. Contain this by centralizing authorization checks in one internal layer that consumes the existing workspace trust signal.
- **Checkpoint compatibility risk:** New scope state could make restore brittle. Contain this by persisting only optional/defaulted restore-relevant fields and rebuilding derived runtime state on restore.

## File-by-File Changes

- `crates/quine-core/src/memory/`
  - Extend the existing memory module tree with scope/policy-focused modules or additive changes to `persistent_memory.rs` / adjacent files.
  - Add the canonical internal types for scope identifiers, lookup order, policy models, permission context, and conflict resolution.
  - Add pure helper functions for scoped path resolution, readable/writable scope selection, authorization, and overlapping-record resolution.
- `crates/quine-core/src/engine.rs`
  - Extend `SessionContext` with additive scoped-memory runtime state.
  - Initialize scoped-memory resolution during session creation/restore.
  - Thread resolved scoped state into prompt-time recall, extraction, and diagnostics paths without changing shared traits.
- `crates/quine-core/src/persistence.rs`
  - Add optional/additively defaulted persisted memory-scope fields needed for restore continuity.
  - Keep serialization backward-compatible and bounded.
- `crates/quine-core/src/permission/`
  - Either extend the existing permission helpers or add a tightly scoped internal memory-permission helper module so memory read/write authorization stays aligned with Quine’s existing trust model.
- `crates/quine-harness/src/config.rs`
  - Add additive memory config fields for advanced-scope flags, policy enforcement flags, root overrides, default keys, lookup order, and write-policy requirements.
  - Keep config parsing/validation harness-owned.
- `crates/quine-harness/src/storage.rs`
  - Extend session-context snapshots with additive scoped-memory diagnostics/inspection fields if Feature 041’s diagnostics snapshot already exists.
  - Keep snapshot fields optional/defaulted for compatibility.
- `crates/quine-harness/src/server.rs` and/or `crates/quine-harness/src/service.rs`
  - Thread any additive per-session `agent_key` / `team_key` or validated memory-policy snapshot fields through session startup and restore paths.
  - Reuse existing request/config flows rather than introducing a new RPC.
- `crates/quine-cli/src/chat.rs` or existing session-start wiring
  - Only if needed, pass additive custom-agent/team identity context through the existing session creation path.
  - Avoid broad CLI UX additions; no new interactive memory-control surface is planned.
- `crates/quine-core/tests/` and relevant unit-test modules
  - Add focused unit and integration coverage for scope resolution, authorization, precedence, conflict handling, and restore behavior.

### Critical implementation files

- `crates/quine-core/src/memory/` — owns the canonical scope/policy rules; most logic risk lives here.
- `crates/quine-core/src/engine.rs` — determines how resolved scope state actually affects prompt recall and extraction during live sessions.
- `crates/quine-harness/src/config.rs` — must keep policy inputs trusted, explicit, and separate from runtime derivation.
- `crates/quine-core/src/persistence.rs` — controls restore compatibility and limits checkpoint growth.
- `crates/quine-harness/src/storage.rs` — likely inspection/diagnostics integration point for proving the feature is working without adding new APIs.

## Validation Plan

- Unit tests in `quine-core` for pure scope/policy logic:
  - scope-path resolution for project-only, project+agent, project+team, and fully enabled cross-scope configurations
  - authorization helpers covering allowed and denied read/write cases by scope
  - explicit-intent requirements for agent/team writes
  - trusted-workspace write requirements
  - conflict-resolution strategies with deterministic tie-breaking
  - serde/default/backward-compat behavior for any newly persisted enums or config snapshots
- Integration tests in `crates/quine-core/tests/` and/or `crates/quine-harness/tests/` for runtime behavior:
  - prompt-time recall reads scopes in the configured order and resolves overlaps deterministically
  - extraction writes exactly one scope and does not mirror the same fact across multiple scopes automatically
  - denied writes do not silently fall back to broader scopes
  - restore rebuilds scoped runtime state correctly from optional persisted fields
  - project isolation remains intact while team and agent scopes map to their configured directories
- Required daemon-backed QA coverage after implementation lands:
  - at least one concrete multi-round local-daemon scenario because `quine-core` prompt behavior changes
  - at least one scenario validating policy denial/authorization behavior through real session interactions
  - additive `/context` or `get_session_context` evidence showing resolved readable scopes, writable scope, and conflict/denial outcomes if diagnostics are available from Feature 041
- Full workspace verification expected for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Reviewed the latest QA plan revision after it was updated with executable shared daemon setup, deterministic test layers, and required evidence. It aligns with the implementation plan’s scope and validation boundaries.
- The paired docs now agree on the key implementation and QA contracts:
  - additive advanced project/team/agent scope handling on top of earlier memory features
  - explicit feature flags and policy enforcement for read/write authorization by scope
  - deterministic lookup order and conflict resolution
  - additive diagnostics and inspection evidence instead of a new memory-management UI or RPC surface
  - no shared inter-crate trait changes
- The QA scenarios now cover the critical implementation risks with enough precision for another agent to execute them directly:
  - project-only reduction when advanced scopes are disabled
  - positive and negative authorization paths
  - overlap and conflict handling across scopes
  - no silent fallback from denied narrow-scope writes to broader project writes
  - live daemon coverage for prompt-time scoped recall and policy denial
  - explicit local-daemon commands, exact chat messages, and exact expected outputs/status/tool activity for the required multi-round `quine-core` scenario
- The QA plan also correctly requires a deterministic local provider or equivalent harness so exact response assertions remain stable during QA execution.
- From the implementation side, no further QA-side changes are needed before implementation begins.
