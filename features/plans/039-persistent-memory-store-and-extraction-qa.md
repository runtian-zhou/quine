# 039 Persistent Memory Store and Durable Extraction — QA Plan

Short summary: Verify Quine’s project-scoped persistent memory store, `MEMORY.md` index maintenance, one-memory-per-file durable entries, and conservative extraction/update/tombstone behavior across restarts and new sessions without yet changing prompt-time recall.

## Open Questions

- None. This QA plan is now concrete enough to execute as written, and it stays within the agreed Feature 3 scope.

## Agreement Status

agreed — I reviewed the latest implementation plan revision, this QA plan now defines executable coverage with concrete daemon-backed scenarios, and there are no unresolved open questions remaining between the paired docs.

## Test Strategy

- Validate this slice in three layers so failures are easy to localize:
  - `quine-harness` path/config tests for durable root resolution, project-key stability, and override precedence.
  - `quine-core` unit and integration tests for frontmatter parsing/rendering, one-memory-per-file persistence, deterministic `MEMORY.md` and `index.json` regeneration, extraction decisions, and persisted extraction boundaries.
  - at least one real local-daemon multi-round test that exercises ordinary chat turns, creates durable memory on disk, updates/tombstones it, restarts the daemon against the same state dir, and proves persistence across a new session in the same project.
- Treat durable on-disk artifacts as the primary observable for this feature. The QA plan does not require a new user-facing memory UI, debug command, or prompt injection behavior.
- Keep assertions conservative and stable:
  - assert exact filesystem paths, file presence/absence, and deterministic index content where the implementation controls formatting;
  - assert exact round-by-round user messages and exact expected response/status/tool activity for the required daemon-backed test by using a deterministic local test provider rather than a network model.
- Require negative coverage for deferred behavior:
  - prompt construction must remain unchanged;
  - durable memory on disk must not by itself change ordinary turn responses in this feature slice;
  - different project roots must not share a memory directory.
- Require restart safety coverage for duplicate suppression:
  - after a turn is extracted, restarting the harness and continuing the same or a new session in the same project must not duplicate the same memory entry or append duplicate index rows.

## Scenarios

### 1. Harness path resolution and project scoping

- **Goal**: Prove durable memory roots resolve under harness-managed state, overrides are additive, and project scoping is stable.
- **Coverage**: unit tests in `crates/quine-harness/src/config.rs` and `crates/quine-harness/src/storage.rs`.
- **Required checks**:
  - default memory root resolves under `<state_dir>/memory/projects/<project_key>/`.
  - trusted memory-root override changes only the durable-memory root, not checkpoint/session-log roots.
  - equivalent project roots normalize to the same `project_key`.
  - two distinct project roots normalize to different `project_key` values.
  - test-only project-key override, if added, is scoped to durable-memory path resolution only.
- **Expected result**: All assertions are deterministic and do not require a live daemon.

### 2. Frontmatter parsing/rendering and one-memory-per-file persistence

- **Goal**: Prove memory entries remain human-readable, parseable, and one-per-file.
- **Coverage**: unit tests in `crates/quine-core/src/memory/`.
- **Required checks**:
  - parse valid frontmatter with `entry_id`, `title`, `summary`, `keywords`, timestamps, source marker, and status marker.
  - reject malformed or incomplete frontmatter cleanly.
  - render then parse returns the same logical record.
  - one live memory record persists to exactly one markdown file under `entries/`.
  - tombstones persist outside `entries/`, never inline inside the live index.
- **Expected result**: round-trip tests are deterministic, and persisted files are inspectable markdown/JSON rather than opaque binary formats.

### 3. Deterministic `MEMORY.md` and `index.json` maintenance

- **Goal**: Prove the generated index stays stable and excludes tombstoned entries.
- **Coverage**: unit tests in `crates/quine-core/src/memory/`.
- **Required checks**:
  - rebuilding from the same live records twice produces byte-identical `MEMORY.md` output.
  - rebuilding from the same live records twice produces byte-identical `index.json` output.
  - live entries are sorted deterministically.
  - tombstoned entries are absent from both `MEMORY.md` and `index.json`.
  - linked paths in `MEMORY.md` point only to `entries/<slug>.md`.
- **Expected result**: deterministic outputs with stable ordering and no index drift.

### 4. Extraction decision logic: explicit remember, explicit forget, and heuristic suppression

- **Goal**: Prove the conservative extraction policy matches the implementation scope.
- **Coverage**: unit tests in `crates/quine-core/src/memory/extract.rs` or equivalent.
- **Required checks**:
  - explicit remember produces a create or update decision.
  - explicit forget produces a tombstone/delete decision against the matching live fact.
  - transient task state is ignored by heuristics.
  - code-derived facts are ignored by heuristics.
  - explicit remember suppresses overlapping heuristic creation for the same fact in the same turn.
  - per-turn extraction cap is enforced deterministically.
- **Expected result**: extraction policy is intentionally conservative and prefers `ignore` over speculative persistence.

### 5. Runtime integration: extraction boundary and best-effort failure handling

- **Goal**: Prove extraction runs only on newly completed history and never fails the user-visible turn.
- **Coverage**: integration tests in `crates/quine-core/tests/` or `crates/quine-harness/tests/` using a deterministic provider and temporary state dir.
- **Required checks**:
  - after the first completed turn, extraction state persists a boundary such as `last_extracted_message_index`.
  - resuming from checkpoint does not re-extract already-processed history.
  - an injected write/index failure records failure state or logs it but still emits normal turn completion.
  - no duplicate memory file or duplicate index row appears after retry/restart.
- **Expected result**: extraction is best-effort maintenance, not part of visible turn success semantics.

### 6. Required multi-round local-daemon test for `quine-core` integration

- **Goal**: Exercise the real daemon/event flow with deterministic responses and on-disk memory artifacts.
- **Implementation requirement**: add one dedicated integration test that launches a local IPC daemon in-process with a deterministic test provider. The test must not depend on network LLM access.
- **Recommended test location**: `crates/quine-harness/tests/persistent_memory_daemon.rs`.
- **Exact command**:
  - `cargo test -p quine-harness persistent_memory_multi_round_local_daemon -- --exact --nocapture`
- **Exact daemon shape required inside the test**:
  - start `run_ipc_server` against a temporary socket path;
  - back it with `LocalHarness::new(Arc::new(EchoProvider), Some(StorageManager::new(<temp_state_dir>)))` or an equivalent deterministic provider that returns the last user message verbatim;
  - create sessions through the JSON-RPC/CLI surface, not by directly mutating core state.
- **Exact project setup required inside the test**:
  - create temporary project root `project-alpha/` with a marker file the implementation uses to resolve the project root;
  - create a second temporary project root `project-beta/` for isolation checks;
  - create sessions whose `working_directory` is `project-alpha/` or `project-beta/` as appropriate.
- **Exact round-by-round chat messages for `project-alpha/`**:
  - Round 1 user message: `Remember this durable preference: always show cargo commands in code fences. Remember this.`
  - Round 2 user message: `Remember this durable preference: always show cargo commands in code fences. Remember this.`
  - Round 3 user message: `Forget this durable preference: always show cargo commands in code fences. Forget this.`
  - Round 4 user message after daemon restart and new session in the same project: `Hello again.`
- **Exact expected visible responses with the deterministic echo provider**:
  - Round 1 final response text: exactly `Remember this durable preference: always show cargo commands in code fences. Remember this.`
  - Round 2 final response text: exactly `Remember this durable preference: always show cargo commands in code fences. Remember this.`
  - Round 3 final response text: exactly `Forget this durable preference: always show cargo commands in code fences. Forget this.`
  - Round 4 final response text: exactly `Hello again.`
- **Exact expected turn status/event behavior for every round**:
  - no `session_error` notification is emitted;
  - one `turn_complete` notification is emitted for each round;
  - no user interaction request is emitted;
  - no tool requests are required or expected from the visible turn flow;
  - if the implementation emits extra internal observability notifications, the test must not depend on them.
- **Exact expected filesystem results after each round in `project-alpha/`**:
  - After Round 1:
    - exactly one project memory directory exists at `<state_dir>/memory/projects/<project_alpha_key>/`;
    - `MEMORY.md` exists;
    - `index.json` exists;
    - `entries/` contains exactly one `*.md` file;
    - `tombstones/` is empty or absent;
    - `MEMORY.md` contains one live entry link to `entries/<slug>.md`;
    - the entry markdown frontmatter records explicit source and live status.
  - After Round 2:
    - `entries/` still contains exactly one `*.md` file;
    - the live entry is updated in place or remains logically identical, but no duplicate second entry file exists;
    - `MEMORY.md` still contains exactly one live entry row for that fact.
  - After Round 3:
    - `entries/` contains no live file for that fact, or the file is marked non-live according to the chosen implementation, but it must no longer appear in `MEMORY.md` or `index.json`;
    - `tombstones/` contains exactly one `*.json` tombstone for the forgotten fact;
    - `MEMORY.md` contains zero live rows for that forgotten fact.
  - After Round 4 and daemon restart/new session in the same project:
    - the same `<project_alpha_key>` directory is reused;
    - no duplicate entry is recreated merely by saying `Hello again.`;
    - `MEMORY.md`, `index.json`, and tombstones remain logically unchanged from post-Round-3 state.
- **Exact project isolation extension in the same test or a sibling test**:
  - send `Remember this durable preference: prefer ripgrep over grep. Remember this.` in a session rooted at `project-beta/`;
  - expected visible response text is exactly the same echoed message;
  - expected filesystem result is a distinct `<project_beta_key>` directory with its own `MEMORY.md`, `index.json`, and exactly one live entry file;
  - `project-alpha` artifacts remain unchanged.

### 7. One-shot CLI smoke scenario against a live local daemon

- **Goal**: Ensure the feature is observable through the user-facing one-shot path without relying on hidden test hooks.
- **Command sequence**:
  - Start daemon in one terminal:
    - `cargo run --bin quine-harness -- start --socket /tmp/quine-memory.sock --state-dir /tmp/quine-memory-state`
  - In a second terminal, `cd` into the target project root so the session inherits the intended project-scoped working directory, then run one-shot messages:
    - `cargo run --bin quine -- run "Remember this durable preference: prefer concise bullet lists. Remember this." --socket /tmp/quine-memory.sock --json`
    - `cargo run --bin quine -- run "Forget this durable preference: prefer concise bullet lists. Forget this." --socket /tmp/quine-memory.sock --json`
- **Expected result**:
  - each command returns JSON containing the normal one-shot success payload for `quine run --json`, including a non-empty final response;
  - the response text does not need to be exact in this manual smoke scenario because the daemon uses the configured provider rather than the deterministic test provider from Scenario 6;
  - after each turn, the operator inspects `/tmp/quine-memory-state/memory/projects/` and confirms the durable store changes match the corresponding explicit remember/forget action for the current project root.
- **Notes**:
  - this is supplementary smoke coverage only;
  - the exact-response requirement is satisfied by Scenario 6, which uses a deterministic local test provider;
  - because the current CLI surface does not expose a dedicated `working_directory` flag for one-shot runs, this scenario must be executed from the intended project directory.

### 8. Negative prompt-behavior regression

- **Goal**: Prove this feature does not inject persistent memory into prompt construction yet.
- **Coverage**: integration test in `crates/quine-core/tests/` or daemon-backed test with deterministic provider that exposes the exact prompt payload to assertions.
- **Required checks**:
  - create a durable memory entry on disk;
  - start a fresh session in the same project;
  - send a neutral prompt with no memory-related language, for example `State only the exact user message you received.` when using an echo/introspecting provider;
  - assert the provider receives only the ordinary system/session context and the new user message, not `MEMORY.md` contents or live memory entry bodies;
  - assert the visible response does not include unexpected memory text.
- **Expected result**: prompt-time recall remains deferred exactly as required by the feature request.

## Required Evidence

- A linked test inventory in the implementation PR description or QA notes showing where each required area is covered:
  - path resolution
  - frontmatter parsing/rendering
  - one-memory-per-file persistence
  - deterministic `MEMORY.md`
  - deterministic `index.json`
  - explicit remember/forget
  - restart/new-session durability
  - duplicate-suppression boundary
  - prompt non-injection regression
  - different-project isolation
- Output from the required local-daemon test command:
  - `cargo test -p quine-harness persistent_memory_multi_round_local_daemon -- --exact --nocapture`
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
- Filesystem evidence captured by assertions or pasted snippets from the temporary state dir showing, at minimum:
  - `memory/projects/<project_key>/MEMORY.md`
  - `memory/projects/<project_key>/index.json`
  - `memory/projects/<project_key>/entries/<slug>.md`
  - `memory/projects/<project_key>/tombstones/<entry_id>.json` for forget coverage
- Evidence that the restart/new-session path uses the same durable project directory and does not duplicate the remembered fact.
- Evidence that no new prompt injection occurred, either from an introspecting provider assertion or equivalent deterministic integration test output.

## Implementation Feedback

- The implementation plan is intentionally scoped correctly for Feature 3 and QA should preserve these boundaries:
  - project-scoped durable memory only
  - no prompt-time `MEMORY.md` injection or targeted recall
  - no team-scoped or agent-scoped memory
  - no shared inter-crate trait contract changes unless an unavoidable implementation detail proves otherwise
- QA coverage should exercise the real harness-managed state layout rather than only internal helpers. At minimum, scenarios should verify on-disk artifacts under a test state dir:
  - `memory/projects/<project_key>/MEMORY.md`
  - `memory/projects/<project_key>/index.json`
  - `memory/projects/<project_key>/entries/<slug>.md`
  - `memory/projects/<project_key>/tombstones/<entry_id>.json` when explicit forget behavior is tested
- Please make restart/new-session verification concrete. The strongest acceptance evidence will show:
  - one session creates durable memory files
  - the harness is restarted against the same state dir
  - a new session in the same project reuses the same project-scoped memory directory without duplicating entries
- Scenarios should not assume a new user-facing memory command exists. This feature is planned as post-turn extraction, so QA should trigger it through ordinary chat turns containing explicit phrases such as “remember this” and “forget this”, then inspect resulting files and index output.
- Because prompt behavior must remain unchanged, at least one scenario should explicitly verify the negative case:
  - durable memory files exist on disk
  - subsequent normal prompts do not show evidence that `MEMORY.md` or entry contents were injected into the model context through any new user-visible prompt path
- Please keep expected results focused on stable, observable artifacts rather than speculative diagnostics. Good checks include:
  - created/updated/tombstoned files on disk
  - deterministic `MEMORY.md` contents and ordering
  - stable `index.json` regeneration
  - no turn failure when extraction fails or is skipped
- Important implementation-sensitive edge cases worth making executable in QA:
  - explicit remember suppresses overlapping heuristic extraction for the same fact in the same turn
  - explicit forget removes or tombstones a live fact and excludes it from regenerated `MEMORY.md`
  - transient or code-derived facts are ignored by heuristics
  - a different project root maps to a different durable memory directory
- After reviewing the latest implementation revision and reconciling repo-accurate CLI/daemon commands, there are no blocking implementation objections remaining. The paired plans now align on concrete daemon-backed coverage, on-disk artifact checks, restart/new-session durability, prompt non-injection, different-project isolation, and executable smoke steps that match the current CLI surface.
