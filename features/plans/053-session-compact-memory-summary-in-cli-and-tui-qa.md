# 053 Session Compact Memory Summary in CLI and TUI — QA Plan

Short summary: Verify that compact session-memory summaries flow through the harness session-context snapshot and are visible to operators in both CLI and TUI session views for live and restored sessions.

## Open Questions

- None. The QA plan assumes this feature is additive to the existing session-context inspection surfaces and does not add a separate command for triggering compaction.

## Agreement Status

agreed — Reviewed against the implementation plan. Both docs align on an additive `compact_memory_summary_markdown: Option<String>` field on `SessionContextSnapshot`, deterministic seeded-session verification, and rendering the same summary through CLI `/context` and the TUI session panel.

## Test Strategy

- Validate the feature at three layers:
  - snapshot creation in core and harness
  - CLI session-context rendering
  - TUI session/context panel rendering
- Prefer deterministic tests that inject a known compact-memory summary into `SessionContextSnapshot` or `PersistedSession.memory_state.compaction.summary_markdown` rather than relying on token thresholds to trigger compaction indirectly.
- Include at least one real local-daemon multi-round scenario because the change affects operator-visible session state that is surfaced through harness-backed chat flows.
- Exercise both live-session and restored-session paths because the compact-memory summary can be sourced from in-memory state and from checkpoint-backed session context.
- Verify absence behavior explicitly so the UI does not show an empty summary section before compaction has produced one.

## Scenarios

- **Scenario 1**: CLI renderer shows compact-memory summary when present
  - **How to start/use local daemon**: No daemon required; use a focused Rust test in `quine-cli` against the session-context renderer.
  - **Exact command**: `cargo test -p quine-cli context_debug::tests::renders_compact_memory_summary -- --exact --nocapture`
  - **Input/setup**: Construct a `SessionContextSnapshot` with a known compact-memory summary string such as `Summary line 1\n- retained fact`.
  - **Expected result**: Rendered CLI output includes a clearly labeled session summary section and includes the exact summary text lines.
- **Scenario 2**: CLI renderer omits summary section when absent
  - **How to start/use local daemon**: No daemon required.
  - **Exact command**: `cargo test -p quine-cli context_debug::tests::omits_compact_memory_summary_when_absent -- --exact --nocapture`
  - **Input/setup**: Construct a `SessionContextSnapshot` without compact-memory summary data.
  - **Expected result**: Rendered output contains no empty `Session Summary` section and all existing diagnostics remain intact.
- **Scenario 3**: TUI state/render helper shows compact-memory summary when present
  - **How to start/use local daemon**: No daemon required.
  - **Exact command**: `cargo test -p quine-cli tui::app::tests::context_panel_includes_compact_memory_summary -- --exact --nocapture`
  - **Input/setup**: Feed the TUI app state a session context snapshot containing a known compact-memory summary.
  - **Expected result**: The rendered or formatted context/session panel contains the exact summary text and label.
- **Scenario 4**: Harness session-context snapshot carries summary for a live session
  - **How to start/use local daemon**: No daemon required for unit/integration coverage; use a harness test that constructs a live session context or live session fixture with `compact_memory_summary_markdown` sourced from in-memory compact-memory state.
  - **Exact command**: `cargo test -p quine-harness local::tests::get_session_context_includes_compact_memory_summary -- --exact --nocapture`
  - **Input/setup**: Seed a live session whose memory snapshot contains `compaction.summary_markdown = "Summary line 1\n- retained fact"`.
  - **Expected result**: `get_session_context()` returns a snapshot with `compact_memory_summary_markdown` populated exactly once and equal to the seeded summary text.
- **Scenario 5**: Restored checkpoint session-context snapshot carries summary
  - **How to start/use local daemon**: No daemon required.
  - **Exact command**: `cargo test -p quine-harness storage::tests::session_context_from_checkpoint_includes_compact_memory_summary -- --exact --nocapture`
  - **Input/setup**: Build a `PersistedSession` or checkpoint fixture containing `memory_state.compaction.summary_markdown = "Summary line 1\n- retained fact"`.
  - **Expected result**: The derived session-context snapshot includes `compact_memory_summary_markdown` with the expected text, proving restored sessions match live sessions.
- **Scenario 6**: Real local-daemon multi-round inspection of session summary
  - **How to start/use local daemon**:
    - Terminal 1: `cargo run --bin quine-harness -- serve --socket /tmp/quine-feature-053.sock`
    - Terminal 2: `cargo run --bin quine -- --socket /tmp/quine-feature-053.sock chat --resume <seeded-session-id>`
  - **Exact setup requirement**:
    - Before running the chat client, create or load a deterministic seeded session/checkpoint using the implementation-provided test fixture/helper so `<seeded-session-id>` already has `compact_memory_summary_markdown = "Summary line 1\n- retained fact"` in its session context source.
  - **Exact round-by-round messages to send**:
    - Round 1 user message: `/context`
    - Expected round 1 response: CLI context output appears successfully with no error text and includes a labeled `Session Summary` section containing exactly `Summary line 1` and `- retained fact`.
    - Round 2 user message: `Please reply exactly with: live session still works`
    - Expected round 2 response: final assistant text is exactly `live session still works`; no error text; normal success status.
    - Round 3 user message: `/context`
    - Expected round 3 response: CLI context output still includes the same labeled `Session Summary` section with the same seeded summary text and no error text.
  - **Expected result**: The live daemon-backed CLI session shows the compact-memory summary through the same session-context flow used by operators.
- **Scenario 7**: Non-interactive context JSON includes additive summary field
  - **How to start/use local daemon**: Use whichever existing non-interactive command path emits `SessionContextSnapshot` JSON, or a focused serialization test if the CLI has no stable one-off command today.
  - **Exact command**: `cargo test -p quine-cli context_debug::tests::serializes_compact_memory_summary_field -- --exact --nocapture`
  - **Input/setup**: Serialize a snapshot with and without `compact_memory_summary_markdown`.
  - **Expected result**: JSON contains `compact_memory_summary_markdown` when present and omits or nulls it consistently when absent without renaming existing fields.

## Required Evidence

- Test output for all focused new or updated unit/integration tests in `quine-core`, `quine-harness`, and `quine-cli`.
- Captured output from the real local-daemon `/context` scenario showing before-and-after session-context inspection, including the visible session summary once present.
- If a deterministic hook or fixture is needed for the live daemon scenario, the QA evidence must record the exact setup command and the exact summary text expected.
- Final repository-wide verification evidence for:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- The implementation plan now names the additive boundary field precisely: `compact_memory_summary_markdown: Option<String>` on the harness `SessionContextSnapshot`; QA should assert that exact field name in serialization and session-context tests.
- The implementation should avoid broad snapshot-shape changes; reuse `PersistedSession.memory_state.compaction.summary_markdown` and the live session memory snapshot as the only data sources.
- The daemon-backed scenario should use a deterministic seeded session or checkpoint resumed via `chat --resume <seeded-session-id>` so QA can verify exact summary text without relying on compaction thresholds.
- The TUI verification should stay helper-level or app-state-level, not a brittle full-screen terminal snapshot, unless the crate already has stable TUI snapshot infrastructure.
