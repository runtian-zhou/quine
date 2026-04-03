# 037 Session Memory Foundation and Summary Lifecycle — QA Plan

Short summary: Verify the first Quine memory-system slice that adds internal memory module groundwork, per-session session-memory bookkeeping, stable `session-memory/summary.md` storage, template initialization, asynchronous post-turn summary updates, and restore-safe metadata handling without yet changing compaction or persistent recall behavior.

## Open Questions

- None from implementation review. QA should validate the agreed assumption that Feature 1 uses deterministic in-core summarization and a JSON sidecar metadata file rather than an internal LLM summarizer or markdown footer parsing.

## Agreement Status

agreed — reviewed the latest implementation-plan revision after completing this QA plan; both docs are aligned on deterministic in-core summarization, harness-state-root storage, async post-turn refresh, recoverable missing-file behavior, and restore-safe persisted metadata, with no unresolved open questions.

## Test Strategy

- Validate the concrete Phase 0 / Feature 1 data-model and internal-API changes from `docs/design/002-memory-systems-design.md`, not just the high-level behavior.
- Explicitly inspect for the implemented forms of:
  - `SessionMemoryPaths`
  - `SessionMemoryState`
  - `SessionSummaryMetadata`
  - `SessionSummaryDocument`
  - `SessionSummaryUpdate`
  - `PersistedMemoryState`
  - `PersistedSessionMemoryState`
- Treat the on-disk session-memory layout and sidecar format as part of the testable internal API contract for this feature.
- Validate the feature at four layers so regressions are easy to localize:
  - focused `quine-core` unit tests for template rendering, refresh-decision logic, sidecar metadata roundtrips, and per-session write-serialization guards
  - `quine-core` integration tests using a mock provider and temporary harness state root for end-to-end summary creation, update, and restore behavior
  - at least one real local-daemon multi-round chat scenario that exercises the actual CLI → harness → core path and inspects session-memory artifacts on disk
  - workspace quality gates to confirm the additive `quine-core`/`quine-harness` changes do not introduce warnings or formatting drift
- Keep QA scoped exactly to Phase 0 groundwork + Feature 1 session memory foundation:
  - verify new internal memory seams exist and stay internal to `quine-core`
  - verify per-session bookkeeping is sufficient for refresh continuation and restore
  - verify `session-memory/summary.md` plus JSON sidecar creation and maintenance
  - verify async refresh is best-effort and does not block the user-visible turn lifecycle
- Treat storage semantics as first-class acceptance criteria:
  - confirm session memory lives under the harness-owned state root rather than the repo working tree
  - confirm raw summary contents are stored only on disk, not duplicated into persisted checkpoints
  - confirm older or missing checkpoint metadata falls back safely
- Prefer daemon-backed scenarios over abstract assertions whenever user-visible behavior is involved.
- For daemon scenarios, record exact commands, exact messages, exact inspected paths, and exact expected turn/tool/error behavior so another agent can replay without inventing details.

## Scenarios

- **Scenario 1: Focused `quine-core` unit coverage for memory helpers**
  - Start/use: run targeted unit tests in the owning modules after implementation lands.
  - Commands:
    - `cargo test -p quine-core memory::template`
    - `cargo test -p quine-core memory::summary`
    - `cargo test -p quine-core memory::session`
    - `cargo test -p quine-core persistence`
  - Exact validation:
    - template tests prove the initial `summary.md` renderer emits the canonical headings agreed in the implementation plan: `Current State`, `Task Specification`, `Files and Functions`, `Workflow`, `Errors & Corrections`, `Codebase and System Documentation`, `Learnings`, `Key Results`, and `Worklog`
    - summary tests prove `summary.meta.json` boundary metadata serializes/deserializes round-trip and rejects malformed or incomplete values predictably
    - refresh-decision tests prove no-op behavior when memory is disabled, a refresh is already in flight, or no new transcript boundary exists
    - session-state tests prove the per-session write-serialization guard allows only one refresh writer at a time for a given session
    - persistence tests prove `PersistedMemoryState` / `PersistedSessionMemoryState` are the only checkpointed session-memory data, with backward-compatible defaults for older checkpoints
  - Expected result: all targeted tests pass and isolate failures to the new memory foundations rather than broader runtime behavior.

- **Scenario 2: `quine-core` integration test for first-turn initialization and second-turn update**
  - Start/use: run the new integration test file promised by the implementation plan against a temp state root and mock provider.
  - Command: `cargo test -p quine-core session_memory_foundation -- --nocapture`
  - Exact validation:
    - after the first completed turn, the test observes creation of `<temp_state_dir>/sessions/<session_id>/session-memory/summary.md`
    - the same directory contains `summary.meta.json`
    - `summary.md` contains the stable template headings rendered from the `SessionSummaryDocument` model rather than an empty or ad hoc file
    - `summary.meta.json` round-trips into `SessionSummaryMetadata`
    - after a second user turn, the sidecar’s last-summarized boundary advances via a new `SessionSummaryUpdate` instead of staying at zero or being rewritten from scratch without progress
    - the updated summary reflects newly summarized transcript material in deterministic structured form
  - Expected result: end-to-end in-process creation and update of session memory succeeds across multiple turns.

- **Scenario 3: Restore/resume integration test continues from persisted boundary**
  - Start/use: run the restore-focused integration test from the same `quine-core` integration suite.
  - Command: `cargo test -p quine-core restore_session_memory_foundation -- --nocapture`
  - Exact validation:
    - the initial harness/core instance processes at least one turn and persists checkpoint metadata that includes durable session-memory state only
    - a restarted harness/core instance restores the same session from checkpoint
    - after sending one additional turn, the memory refresh resumes from the persisted boundary instead of resetting to “unsummarized” or duplicating previously summarized work
    - if the summary files are absent when the restored session resumes, the next eligible refresh recreates them successfully instead of failing the session
  - Expected result: restore behavior preserves enough metadata to continue maintenance correctly.

- **Scenario 4: Missing-file recovery without user-visible chat failure**
  - Start/use: execute either a dedicated integration test or a daemon-backed manual scenario after one successful summary refresh.
  - Preferred command: `cargo test -p quine-core missing_session_memory_files_are_recovered -- --nocapture`
  - If manual daemon execution is used instead, delete one of the generated files between turns and then send another message.
  - Exact validation:
    - deleting `summary.md` alone results in recreation from template plus refreshed content on the next eligible turn
    - deleting `summary.meta.json` alone results in sidecar recreation with a valid machine-readable boundary
    - neither deletion causes the chat turn itself to fail, hang, or emit a fatal session error
  - Expected result: missing session-memory artifacts are treated as recoverable state, not as fatal corruption.

- **Scenario 5: Multi-round local-daemon chat flow creates and updates session memory**
  - Start/use: run against the real daemon with an isolated socket and state directory so the created paths are deterministic and disposable.
  - Daemon start command:
    - `cargo run --bin quine-harness -- start --socket /tmp/quine-memory-037.sock --state-dir /tmp/quine-memory-037-state`
  - Round 1 command:
    - `cargo run --bin quine -- run --json --socket /tmp/quine-memory-037.sock "Please reply with exactly: SESSION MEMORY ROUND 1"`
  - Expected Round 1 output:
    - stdout is JSON with keys `session_id`, `response`, and `tool_calls`
    - `response` equals exactly `SESSION MEMORY ROUND 1`
    - `tool_calls` is `[]`
    - stderr includes `session: <session_id>`
  - Session discovery command if needed:
    - `cargo run --bin quine -- ps --json --socket /tmp/quine-memory-037.sock`
  - Filesystem inspection commands after Round 1:
    - `find /tmp/quine-memory-037-state -path "*/session-memory/*" -print | sort`
    - `cat /tmp/quine-memory-037-state/sessions/<session_id>/session-memory/summary.md`
    - `cat /tmp/quine-memory-037-state/sessions/<session_id>/session-memory/summary.meta.json`
  - Expected post-Round 1 state:
    - the directory `/tmp/quine-memory-037-state/sessions/<session_id>/session-memory/` exists
    - `summary.md` exists with the canonical section headings
    - `summary.meta.json` exists and records a non-empty last-summarized boundary or equivalent marker for the first turn
  - Round 2 command:
    - `cargo run --bin quine -- run --json --socket /tmp/quine-memory-037.sock --session <session_id> "Please reply with exactly: SESSION MEMORY ROUND 2"`
  - Expected Round 2 output:
    - stdout JSON `response` equals exactly `SESSION MEMORY ROUND 2`
    - `tool_calls` remains `[]`
    - no stdout/stderr text indicates a memory-maintenance failure
  - Filesystem inspection commands after Round 2:
    - `cat /tmp/quine-memory-037-state/sessions/<session_id>/session-memory/summary.md`
    - `cat /tmp/quine-memory-037-state/sessions/<session_id>/session-memory/summary.meta.json`
  - Expected post-Round 2 state:
    - `summary.md` still has the template structure and now reflects both rounds in deterministic summary content or worklog updates
    - `summary.meta.json` shows the summarized boundary advanced relative to the value captured after Round 1
    - no `MEMORY.md` or cross-session durable-memory files are created anywhere under `/tmp/quine-memory-037-state/sessions/<session_id>/`
  - Expected status/tool/error behavior across both rounds:
    - each command completes normally with a user-visible final response
    - no tool activity occurs for these echo-style prompts
    - no fatal `session_error` event is emitted

- **Scenario 6: Daemon-backed proof that summary refresh is asynchronous relative to turn completion**
  - Start/use: reuse the daemon from Scenario 5 and inspect the session log produced by the harness.
  - Chat command:
    - `cargo run --bin quine -- run --json --socket /tmp/quine-memory-037.sock --session <session_id> "Please reply with exactly: ASYNC MEMORY CHECK"`
  - Log inspection command:
    - `cargo run --bin quine -- log <session_id> --socket /tmp/quine-memory-037.sock`
  - Exact validation:
    - the chat command returns a successful final response `ASYNC MEMORY CHECK`
    - the log contains a normal `turn_complete` event for that turn
    - if any memory-maintenance logging or follow-up checkpoint/session events are emitted, they occur without preventing or delaying the successful turn completion
    - there is no user-visible error requiring retry, and the summary files update shortly after the turn completes
  - Expected result: the turn lifecycle remains healthy even though memory maintenance runs as best-effort background work.
  - Note: QA should not require a dedicated new user-visible event for memory refresh; the pass condition is that turn completion remains normal while session-memory files advance shortly afterward.

- **Scenario 7: Daemon-backed resume after shutdown continues summary maintenance**
  - Start/use: create a session, complete at least one turn, shut down the daemon, restart it against the same state root, and resume the session.
  - Initial daemon command:
    - `cargo run --bin quine-harness -- start --socket /tmp/quine-memory-037-resume.sock --state-dir /tmp/quine-memory-037-resume-state`
  - Initial turn command:
    - `cargo run --bin quine -- run --json --socket /tmp/quine-memory-037-resume.sock "Please reply with exactly: RESUME ROUND 1"`
  - Shutdown command:
    - stop the daemon process cleanly with `Ctrl-C` or the project’s normal shutdown path
  - Restart daemon command:
    - `cargo run --bin quine-harness -- start --socket /tmp/quine-memory-037-resume.sock --state-dir /tmp/quine-memory-037-resume-state`
  - Discover resumable sessions command:
    - `cargo run --bin quine -- ps --json --socket /tmp/quine-memory-037-resume.sock`
  - Resumed turn command:
    - `cargo run --bin quine -- run --json --socket /tmp/quine-memory-037-resume.sock --resume latest "Please reply with exactly: RESUME ROUND 2"`
  - Expected results:
    - the resumed command targets the previously created session rather than creating an unrelated new one
    - the final response is exactly `RESUME ROUND 2`
    - `/tmp/quine-memory-037-resume-state/sessions/<session_id>/session-memory/summary.meta.json` shows a later summarized boundary after Round 2 than after Round 1
    - the summary survives daemon restart because the path is anchored in harness state, not the working tree
    - no duplicate cross-session memory store is created

- **Scenario 8: Workspace regression gates**
  - Start/use: run the required workspace verification commands after targeted tests are green.
  - Commands:
    - `cargo build`
    - `cargo test`
    - `cargo clippy --all-targets -- -D warnings`
    - `cargo fmt --all -- --check`
  - Exact validation:
    - the memory feature introduces no build, test, lint, or formatting regressions
    - no inter-crate trait contract change was required to make the feature work
  - Expected result: the implementation remains additive, formatted, and warning-free.

## Required Evidence

- Passing focused tests for:
  - `SessionMemoryState` initialization and refresh-decision logic
  - `SessionSummaryDocument` template rendering
  - `SessionSummaryMetadata` parsing/serialization
  - `SessionSummaryUpdate` boundary advancement
  - `PersistedMemoryState` / `PersistedSessionMemoryState` checkpoint serde and restore defaults
  - per-session write serialization or equivalent race prevention
- Passing `quine-core` integration evidence showing:
  - `summary.md` and `summary.meta.json` are created under the stable harness state root session path
  - a later turn advances the summary boundary rather than recreating state from scratch
  - restore/resume continues maintenance correctly
  - missing-file recovery succeeds without breaking the session
- At least one recorded multi-round local-daemon QA run with the exact command transcript from Scenario 5, including:
  - daemon start command
  - round-by-round CLI invocations
  - the exact final response text for each round
  - the session id used for filesystem inspection
  - the inspected `summary.md` and `summary.meta.json` paths under the chosen state root
- Evidence from session logs or equivalent observation that:
  - the user-visible turn completes normally
  - no fatal session error is emitted for memory maintenance
  - summary files update asynchronously/best-effort after turn completion
- Confirmation that non-goals remain untouched:
  - no compaction behavior is required or exercised for acceptance of this feature
  - no prompt-time session-memory injection is added
  - no persistent cross-session `MEMORY.md` or durable-memory files are created by this feature slice
- Passing workspace quality gates:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Scope alignment: keep this feature strictly to the Phase 0 / Feature 1 data structures and internal APIs from `docs/design/002-memory-systems-design.md`, plus the associated `session-memory/summary.md` + sidecar lifecycle. Do not include compaction consuming session memory, prompt-time memory injection, or cross-session durable memory.
- Storage location: expected session-memory path is under the harness-owned state root, not the repo working tree. The implementation plan targets `<state_dir>/sessions/<session_id>/session-memory/summary.md` plus `summary.meta.json`.
- Summarization model: Feature 1 is planned around deterministic in-core summarization, not a separate internal LLM call. QA scenarios should therefore validate file structure, boundary advancement, and continuity behavior rather than semantic equivalence to an LLM-generated summary.
- Async behavior: include at least one scenario that proves the user-visible turn completes normally before or independently of summary maintenance. Evidence should show that chat output/turn completion succeeds even if summary refresh happens just afterward.
- Missing-file recovery: include a scenario where `summary.md` or `summary.meta.json` is absent after initial creation and the next eligible turn recreates or repairs it without breaking the session.
- Restore coverage: include a checkpoint/restore scenario that verifies persisted memory metadata resumes from the last summarized boundary instead of starting over.
- Race prevention: include evidence for per-session write serialization, ideally via a focused `quine-core` integration or unit test rather than a brittle manual daemon race.
- Local-daemon coverage: because this change touches `quine-core`, at least one multi-round daemon scenario should exercise real chat flow. The scenario should send at least two rounds, then inspect the resulting state-root session-memory files.
- Observability: QA should record the exact filesystem paths inspected and the exact commands used to locate the current session id and harness state directory so another agent can rerun the scenario without guessing.
- Non-goal enforcement: expected results should explicitly confirm no compaction-specific behavior change is under test and no persistent `MEMORY.md` files are created by this feature.
