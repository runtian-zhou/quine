# 041 Memory Diagnostics and Operator Visibility — QA Plan

Short summary: Verify structured read-only memory diagnostics, additive inspection/session snapshot exposure, CLI/debug visibility where applicable, and sufficient structured output for QA to explain why memory was refreshed, injected, skipped, truncated, or ignored.

## Open Questions

- None. The implementation plan is testable as written if the feature remains additive, read-only, and centered on `get_session_context` / `/context` exposure for latest-turn diagnostics.

## Agreement Status

agreed — this QA plan has been cross-checked against the latest implementation plan revision, the implementation plan is now specific enough to validate without inventing missing steps, and there are no unresolved open questions.

## Test Strategy

- Validate this feature at four layers so QA can prove visibility without changing memory semantics:
  - `quine-core` unit coverage for diagnostics data shapes, enum/status stability, and bounded serialization.
  - `quine-core` integration coverage for latest-turn diagnostics capture across session-memory refresh, compaction fallback/source selection, prompt-time persistent-memory recall, and post-turn extraction summaries.
  - `quine-harness` snapshot coverage proving `get_session_context` exposes diagnostics additively and remains backward-compatible when memory diagnostics are absent or partially populated.
  - `quine-cli` coverage proving `/context` / context-debug output can deserialize and render the new snapshot fields without depending on ad hoc debug logs.
- Treat the JSON-serializable diagnostics payload as the contract. Human-readable CLI rendering is secondary and should only be tested insofar as it faithfully surfaces the same structured data.
- Keep validation read-only. No scenario should require a diagnostics-triggered mutation flow, repair command, summary rebuild, or manual memory edit through a new operator interface.
- Prefer deterministic fixtures and temporary memory roots so selected/skipped entries, reason enums, and truncation behavior can be asserted without relying on ambient user state.
- Because this feature affects `quine-core`, require at least one multi-round local-daemon scenario that exercises a live session, then inspects the same session with `/context` immediately after the relevant turn.
- For all end-to-end checks, assert both positive and negative visibility:
  - a turn where memory activity occurs and diagnostics explain what happened
  - a turn where memory work is skipped, falls back, or yields no selected entries and diagnostics explain why

## Scenarios

- `Scenario 1 — Diagnostics model serialization stability`
  - **Purpose**: Prove the canonical diagnostics types are structured, bounded, and serde-stable for automated QA.
  - **Target**: `crates/quine-core` unit tests in the diagnostics module.
  - **Exact command**: `cargo test -p quine-core memory::diagnostics -- --nocapture`
  - **Expected result**:
    - Tests pass.
    - Snapshot/assertion coverage verifies enum-backed reason/status values rather than free-form prose.
    - Serialized payload omits raw memory bodies and remains bounded to latest-turn metadata plus selected/skipped entry summaries.
  - **Required assertions**:
    - `MemoryTurnDiagnostics` serializes/deserializes round-trip.
    - Session-memory status/reason enums cover at least refreshed, skipped, failed-best-effort, stale, missing-path, invalid-boundary, and fallback cases when applicable to the final implementation.
    - Persistent-memory selection/skip diagnostics serialize with stable machine-readable reason codes for threshold, duplicate, stale, truncation, budget, or disabled cases when applicable.

- `Scenario 2 — Additive harness snapshot exposure with no memory activity yet`
  - **Purpose**: Prove backward-compatible session snapshots for a fresh session before any memory maintenance runs.
  - **Target**: `crates/quine-harness` tests around `SessionContextSnapshot` / `get_session_context`.
  - **Exact command**: `cargo test -p quine-harness session_context -- --nocapture`
  - **Expected result**:
    - Tests pass.
    - A fresh session snapshot still deserializes successfully.
    - The new diagnostics field is present as an additive field and is either `null`, absent via `Option`, or populated with an explicit `no_activity_yet` / disabled-style structured state, depending on the final schema.
  - **Required assertions**:
    - Existing snapshot fields keep their meaning.
    - No required consumer field is removed or renamed.
    - Partial diagnostics payloads deserialize successfully in CLI-side snapshot types.

- `Scenario 3 — Session-memory refresh diagnostics in-process`
  - **Purpose**: Prove latest-turn diagnostics record whether session memory refreshed, skipped, or failed best-effort without changing turn success semantics.
  - **Target**: `quine-core` tests covering the session-memory maintenance path.
  - **Exact command**: `cargo test -p quine-core session_memory -- --nocapture`
  - **Expected result**:
    - Tests pass.
    - A successful agent turn still succeeds when refresh succeeds, is skipped, or fails best-effort.
    - Diagnostics capture the resolved summary path/boundary metadata and structured reason/status.
  - **Required assertions**:
    - Refresh-attempted vs skipped is explicit.
    - Boundary/source metadata is recorded when known.
    - Failure is surfaced in diagnostics without converting the turn into a session error.

- `Scenario 4 — Persistent-memory selection and skip diagnostics in-process`
  - **Purpose**: Prove prompt-time durable-memory injection records both selected and skipped entries with structured reasons.
  - **Target**: `quine-core` tests covering targeted recall/index injection.
  - **Exact command**: `cargo test -p quine-core persistent_memory -- --nocapture`
  - **Expected result**:
    - Tests pass.
    - Diagnostics identify whether prompt-time injection ran.
    - Selected entries are listed in bounded form.
    - Skipped entries include structured reasons instead of unstructured log text.
  - **Required assertions**:
    - At least one test covers no selected entries.
    - At least one test covers skip reasons such as stale, duplicate, threshold, truncation, or budget, whichever apply to the implementation.
    - Diagnostics do not embed full durable entry bodies.

- `Scenario 5 — Compaction source and fallback diagnostics in-process`
  - **Purpose**: Prove diagnostics explain whether compaction continuity came from session memory or a legacy summarizer path and why fallback happened.
  - **Target**: `quine-core` compaction-related tests.
  - **Exact command**: `cargo test -p quine-core compact -- --nocapture`
  - **Expected result**:
    - Tests pass.
    - Diagnostics distinguish source selection from fallback reason.
  - **Required assertions**:
    - Session-memory-backed compaction is distinguishable from fallback.
    - A fallback case records a structured reason rather than disappearing from the payload.

- `Scenario 6 — Post-turn extraction outcome diagnostics in-process`
  - **Purpose**: Prove post-turn persistent-memory extraction outcomes are visible in latest-turn diagnostics.
  - **Target**: `quine-core` memory extraction tests.
  - **Exact command**: `cargo test -p quine-core extraction -- --nocapture`
  - **Expected result**:
    - Tests pass.
    - Diagnostics summarize create/update/tombstone/ignore/no-op outcomes where those concepts exist in the final implementation.
  - **Required assertions**:
    - The payload is summary-oriented and bounded.
    - Extraction outcome visibility does not imply a new mutation control surface.

- `Scenario 7 — CLI context debug compatibility`
  - **Purpose**: Prove the CLI context snapshot types and renderer tolerate the additive diagnostics fields.
  - **Target**: `crates/quine-cli` tests around `context_debug` and any adjacent TUI snapshot rendering tests that already exist.
  - **Exact command**: `cargo test -p quine-cli context_debug -- --nocapture`
  - **Expected result**:
    - Tests pass.
    - The CLI can deserialize snapshots containing the new diagnostics payload.
    - Rendering remains readable and does not panic when diagnostics are absent, partial, or populated.
  - **Required assertions**:
    - `/context` JSON output continues to be the primary assertion surface.
    - Renderer summaries, if added, must correspond to the structured fields.

- `Scenario 8 — Multi-round local-daemon session-memory visibility`
  - **Purpose**: Satisfy the required live `quine-core` multi-round daemon test and prove latest-turn diagnostics are observable through the existing operator surface.
  - **Setup command**:
    - Terminal A: `cargo run --bin quine -- daemon start --socket /tmp/quine-feature-041.sock`
  - **Interaction command**:
    - Terminal B: `cargo run --bin quine -- chat --socket /tmp/quine-feature-041.sock`
  - **Exact round-by-round chat messages**:
    - Round 1 user message: `Please acknowledge this exact phrase and do nothing else: FEATURE-041-ROUND-1`
    - Round 1 expected assistant result:
      - Final response text contains `FEATURE-041-ROUND-1`.
      - No session error is emitted.
      - Tool activity is either none or unrelated to memory diagnostics; the feature must not introduce a new tool call just to gather diagnostics.
    - Round 2 user message: `/context`
    - Round 2 expected assistant/result surface:
      - The CLI prints the JSON session-context snapshot.
      - The snapshot includes the additive memory diagnostics field.
      - The latest-turn diagnostics describe Round 1, not an invented extra turn.
      - Expected status behavior:
        - session state remains healthy/active
        - no error text is printed
      - Expected diagnostics content:
        - a session-memory diagnostics object is present if session memory is enabled for the session
        - it records whether refresh ran, skipped, or failed best-effort
        - it includes structured status/reason fields and any resolved summary path or boundary metadata available after Round 1
        - if no session-memory work occurred, the diagnostics must explicitly say so in structured form rather than omitting the section ambiguously
    - Round 3 user message: `/quit`
    - Round 3 expected result:
      - Chat exits cleanly with no daemon-side error.
  - **Required evidence**:
    - Full Terminal B transcript with the exact messages above.
    - Captured `/context` JSON showing the latest-turn diagnostics payload.
    - Confirmation that `/context` did not mutate memory state or trigger a new memory refresh.

- `Scenario 9 — Multi-round local-daemon skipped/fallback visibility`
  - **Purpose**: Prove the live operator surface explains a skip/fallback/no-selection path, not only a successful memory update path.
  - **Setup commands**:
    - Terminal A: `cargo run --bin quine -- daemon start --socket /tmp/quine-feature-041.sock`
    - Terminal B: `cargo run --bin quine -- chat --socket /tmp/quine-feature-041.sock`
  - **Exact round-by-round chat messages**:
    - Round 1 user message: `Answer with exactly: FEATURE-041-ROUND-2`
    - Round 1 expected assistant result:
      - Final response text contains exactly `FEATURE-041-ROUND-2` or the implementation’s standard minimal wrapper around that phrase.
      - No error text is printed.
    - Round 2 user message: `/context`
    - Round 2 expected assistant/result surface:
      - The JSON session-context snapshot includes memory diagnostics for the latest completed turn.
      - At least one diagnostics subsection shows a negative path in structured form: skipped refresh, no selected durable memories, fallback source, truncation, disabled subsystem, or equivalent bounded reason from the implementation.
      - The negative-path reason appears as a structured enum/string code field, not only as prose.
      - No mutation or repair action is offered.
    - Round 3 user message: `/quit`
    - Round 3 expected result:
      - Chat exits cleanly.
  - **Execution note**:
    - If the default local environment does not reliably produce a negative path, QA must create a deterministic fixture-backed environment for this same scenario, such as an empty or deliberately bounded memory root configured before daemon start, but the live validation must still occur through the same daemon + `/context` operator path.

- `Scenario 10 — One-shot session inspection after a memory-affecting turn`
  - **Purpose**: Prove the existing one-shot flow and session snapshot inspection can be combined without a new protocol surface.
  - **Setup commands**:
    - Start daemon: `cargo run --bin quine -- daemon start --socket /tmp/quine-feature-041.sock`
    - Send one-shot turn: `cargo run --bin quine -- run --json --socket /tmp/quine-feature-041.sock "Summarize this token exactly once: FEATURE-041-ONESHOT"`
  - **Expected output of one-shot command**:
    - JSON includes `session_id`.
    - JSON includes final `response` containing `FEATURE-041-ONESHOT`.
    - JSON may include `tool_calls`, but the feature must not require a dedicated diagnostics tool.
    - No session error occurs.
  - **Follow-up inspection command**:
    - `printf '/context\n/quit\n' | cargo run --bin quine -- chat --socket /tmp/quine-feature-041.sock --resume latest`
  - **Expected follow-up output**:
    - The `/context` JSON includes the same resumed session and latest-turn diagnostics for the one-shot turn.
    - The diagnostics field remains additive and read-only.

## Required Evidence

- `Unit and integration evidence`:
  - Exact output from the targeted crate-level commands listed above.
  - For any newly added snapshot tests, include the asserted JSON shape or key excerpt in the implementation PR description or QA notes.
- `Daemon evidence`:
  - The exact daemon start command used.
  - Full command transcripts for the multi-round scenarios, including every user message, relevant assistant text, `/context` output, and clean shutdown/exit behavior.
  - The captured `/context` JSON excerpt showing:
    - latest-turn diagnostics object
    - session-memory status/reason fields
    - persistent-memory selected/skipped/extraction summary fields as applicable
    - fallback/skip/no-activity representation
- `Behavioral evidence`:
  - Proof that a turn can succeed even when a memory maintenance path is skipped or best-effort-fails.
  - Proof that diagnostics visibility does not add mutation APIs, repair flows, or new required tool activity.
  - Proof that diagnostics are bounded and do not expose raw summary markdown or full durable-memory file contents.
- `Workspace evidence`:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
- `Pass criteria`:
  - QA can explain, from structured snapshot fields alone, whether session memory refreshed, which source/boundary path applied, whether persistent-memory injection/extraction ran, which entries were selected or skipped, and why fallback/truncation/stale/disabled outcomes occurred.
  - All validation uses additive inspection surfaces and leaves runtime memory semantics unchanged.

## Implementation Feedback

- Implementation review complete. The QA plan should stay centered on the existing `get_session_context` / `/context` inspection path rather than assuming a brand-new RPC method or standalone memory-debug command; that is the least risky operator surface in the current codebase.
- Please require explicit coverage for four diagnostic categories, because the implementation is likely to source them from different code paths:
  - session-memory refresh/update diagnostics
  - compaction source/fallback diagnostics
  - prompt-time persistent-memory injection/targeted-recall diagnostics
  - post-turn persistent-memory extraction outcome diagnostics
- For daemon-backed scenarios, prefer checking structured fields returned by `/context` or `get_session_context` rather than depending on human-readable renderer prose. The contract for this feature should be the JSON-serializable diagnostics payload.
- The QA plan should require at least one scenario where memory work is skipped or falls back, not just a successful update/injection path. This feature is specifically about explaining why memory was skipped, stale, truncated, or ignored.
- Because current CLI chat already exposes `/context`, the exact scenario steps should specify:
  - how to start the local daemon or `quine chat`
  - the exact messages to send to produce a deterministic memory event
  - when to run `/context`
  - the expected structured fields that must be present, including reason/status values
- Keep the plan explicitly read-only. QA should not require any repair or mutation workflow such as rebuilding summaries, forcing refreshes, or editing memory files through a diagnostics interface.
- Please also ask for coverage of the “no memory activity yet” state so we verify additive snapshot compatibility for fresh sessions and avoid regressions where diagnostics are missing or null in surprising ways.
- If the implementation keeps diagnostics runtime-only rather than checkpoint-persisted, QA should validate them immediately after the relevant turn in the same live session instead of assuming restart persistence for latest-turn details.
