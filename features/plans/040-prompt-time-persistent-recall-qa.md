# 040 Prompt-Time Persistent Memory Injection and Recall — QA Plan

Short summary: Verify bounded baseline `MEMORY.md` injection, deterministic targeted durable-memory recall, prompt-ordering stability, budget/truncation behavior, and disabled-mode no-op behavior for Quine prompt construction.

## Open Questions

- None. After reviewing `features/040-prompt-time-persistent-recall.md`, `features/plans/040-prompt-time-persistent-recall-implementation.md`, `docs/design/002-memory-systems-design.md`, `CLAUDE.md`, the planning-command requirements, and the current `quine-core` prompt/session-context surfaces, the remaining QA work is concrete and executable without additional clarification.

## Agreement Status

agreed — I reviewed the latest implementation-plan revision after completing this QA plan, the paired docs now align on prompt-time baseline `MEMORY.md` index injection plus deterministic targeted project-scoped recall only, and there are no unresolved open questions.

## Test Strategy

- Validate this slice at four layers so failures localize cleanly:
  - focused `quine-core` unit tests for prompt-memory mode selection, `MEMORY.md` ordering, deterministic candidate ranking, thresholding, de-duplication, and truncation/budget accounting
  - `quine-core` integration tests that build provider requests from realistic session history and persistent-memory fixtures without going through the full daemon
  - at least one local-daemon multi-round test that exercises CLI/IPC → harness → core prompt construction and inspects post-turn context/debug evidence
  - full workspace quality gates required by `CLAUDE.md`
- Keep QA strictly within Feature 040 scope:
  - bounded baseline `MEMORY.md` injection in deterministic prompt order
  - deterministic targeted recall from project-scoped durable memory only
  - prompt-budget and truncation behavior
  - disabled-mode no-op behavior
  - additive inspection of the last prompt-memory injection only if the implementation lands that surface
- Treat the following as first-class observable contracts:
  - `IndexOnly` extends the baseline prompt-prefix path and includes an explicit stale-memory caveat
  - `TargetedRecall` injects only selected durable memories for the active turn and does not silently fall back to broad index injection when nothing matches
  - selected targeted memories are ordered deterministically and bounded by centralized budget rules
  - prompt-memory artifacts are ephemeral to the turn and do not persist into `history`
  - disabled mode preserves current prompt behavior apart from an empty/disabled summary state if additive inspection is implemented
- Prefer stable evidence over brittle prose matching:
  - exact provider-request assertions in unit/integration tests when using an introspecting deterministic test provider
  - exact `get_session_context` or `/context` JSON assertions only for additive summary fields and history shape, not for unrelated session fields
  - exact round-by-round daemon expectations for final text, `turn_complete` / `session_error` behavior, and any tool activity
- Do not require new rich diagnostics, UI affordances, advanced scopes, or LLM-based retrieval to execute QA.

## Scenarios

### 1. `quine-core` unit coverage for baseline index injection ordering

- **Goal**: Prove `IndexOnly` mode deterministically injects bounded `MEMORY.md` content through the prefix/system-prompt path and includes the stale-memory caveat.
- **Start/use**: run focused unit tests in the new prompt-memory module and/or `engine.rs` helpers.
- **Commands**:
  - `cargo test -p quine-core prompt_memory_index_only_injection_order -- --exact --nocapture`
  - `cargo test -p quine-core prompt_memory_index_only_truncates_memory_md_by_budget -- --exact --nocapture`
- **Fixture requirements**:
  - synthesize a project-scoped persistent-memory directory containing `MEMORY.md` whose content is unambiguous and longer than one paragraph
  - include at least one line near the truncation boundary so the test can prove deterministic clipping and warning/caveat placement
- **Exact validation**:
  - the rendered combined system prompt keeps existing ordering intact: plan-mode prefix when enabled, then `CLAUDE.md`, then explicit base system prompt / skill prompts, then injected `MEMORY.md` block in the implementation-defined position described by the plan
  - the injected block is byte-stable for the same fixture on repeated runs
  - the injected block contains an explicit stale-memory caveat telling the model to verify durable facts against the repository and current instructions
  - when `MEMORY.md` exceeds the configured budget, injected text is truncated deterministically and records truncation in the returned prompt-memory summary
  - no targeted reminder messages are produced in `IndexOnly` mode
- **Expected result**: tests pass and isolate ordering/budget failures to the baseline injection path.

### 2. `quine-core` unit coverage for deterministic targeted recall ranking

- **Goal**: Prove targeted recall selects the same relevant entries in the same order from the same fixtures every time.
- **Start/use**: run focused unit tests for candidate discovery, scoring, tie-breaking, thresholding, and de-duplication.
- **Commands**:
  - `cargo test -p quine-core prompt_memory_targeted_recall_ranks_by_overlap_recency_and_pin -- --exact --nocapture`
  - `cargo test -p quine-core prompt_memory_targeted_recall_excludes_memory_md_and_unmatched_entries -- --exact --nocapture`
  - `cargo test -p quine-core prompt_memory_targeted_recall_deduplicates_previously_selected_entry_ids -- --exact --nocapture`
- **Fixture requirements**:
  - create at least four durable memory entries plus `MEMORY.md`
  - make the latest user message exactly `What terminal command should I use to run the Rust test suite?`
  - give the entries intentionally different metadata:
    - entry A: title/summary/keywords strongly overlap `Rust`, `test`, and `cargo test`
    - entry B: weaker lexical overlap but newer timestamp
    - entry C: similar overlap to B but `pinned: true`
    - entry D: unrelated content such as design-preference prose
  - assign stable `entry_id` values so tie-breaking can be asserted exactly
- **Exact validation**:
  - `MEMORY.md` is never considered a targeted candidate
  - selected entries are returned in deterministic order based on the implementation contract: highest score, then pinned, then most recent `updated_at`, then `entry_id`
  - unrelated entry D is excluded
  - repeated selection in the same runtime context de-duplicates previously surfaced `entry_id`s
  - no selection path silently falls back to index injection
- **Expected result**: tests pass and prove targeted recall is deterministic, bounded, and explainable.

### 3. `quine-core` integration coverage for prompt assembly in `TargetedRecall` mode

- **Goal**: Prove the final provider request contains ephemeral reminder messages in the correct place and that they are not persisted into session history.
- **Start/use**: run a `quine-core` integration test using a deterministic introspecting provider that captures the exact outbound request message list.
- **Command**:
  - `cargo test -p quine-core targeted_recall_prompt_assembly_inserts_ephemeral_reminders_before_latest_user_message -- --exact --nocapture`
- **Fixture requirements**:
  - create a session history with one system message, at least one prior user/assistant exchange, and a newest user message of `What should I run to execute all tests in this repo?`
  - seed the persistent-memory fixture with two clearly relevant entries and one irrelevant entry
- **Exact validation**:
  - the outbound provider request includes zero or more synthetic reminder messages immediately before the newest user message, not appended after it and not merged into older transcript content
  - the reminder message bodies reference only the selected relevant entries and respect per-entry truncation limits
  - after the turn completes, persisted/live `SessionContext.history` contains only the ordinary transcript messages for the system prompt, user turn, assistant reply, and any normal tool artifacts; it does not retain the synthetic reminder messages
  - the additive last-injection summary, if implemented, reports `TargetedRecall`, the selected `entry_id`s, and any skipped/truncated reasons predictably
- **Expected result**: prompt assembly uses ephemeral turn-local recall without mutating stored conversation history.

### 4. `quine-core` integration coverage for disabled mode no-op behavior

- **Goal**: Prove prompt construction is behaviorally unchanged when prompt-memory injection is disabled.
- **Start/use**: run a provider-request snapshot or equivalence test that compares disabled mode against current baseline construction.
- **Commands**:
  - `cargo test -p quine-core prompt_memory_disabled_mode_is_request_equivalent_to_legacy_path -- --exact --nocapture`
  - `cargo test -p quine-core prompt_memory_disabled_mode_records_empty_or_disabled_summary_only -- --exact --nocapture`
- **Fixture requirements**:
  - use the same working directory, `CLAUDE.md`, skills, system prompt, history, and persistent-memory fixture in both branches
- **Exact validation**:
  - the outbound provider request message list is byte-for-byte identical to the legacy path when the feature is disabled
  - no `MEMORY.md` text or memory-entry body appears in the request
  - if session-context inspection gains a prompt-memory summary field, it reports a disabled/empty state only and does not list selected entries
- **Expected result**: disabled mode remains a true no-op for prompt construction.

### 5. `quine-core` integration coverage for truncation and no-match behavior

- **Goal**: Prove targeted recall respects budget ceilings and injects nothing when the latest user message has no qualifying match.
- **Start/use**: run integration tests with oversize entry bodies and a neutral user query.
- **Commands**:
  - `cargo test -p quine-core targeted_recall_enforces_entry_and_total_budget_caps -- --exact --nocapture`
  - `cargo test -p quine-core targeted_recall_injects_nothing_when_no_candidates_pass_threshold -- --exact --nocapture`
- **Fixture requirements**:
  - prepare three relevant entries whose full bodies collectively exceed the total reminder budget
  - prepare a second query with intentionally no overlap, for example `Tell me a joke about databases.`
- **Exact validation**:
  - only the highest-ranked entries that fit the total budget are injected
  - oversized selected entries are truncated deterministically according to the configured per-entry limit
  - skipped entries are recorded with explicit reasons such as `budget`, `threshold`, or `duplicate` if the implementation exposes summary details
  - the no-match query results in zero reminder messages and zero fallback broad-index injection
- **Expected result**: truncation is deterministic and empty recall stays empty.

### 6. Required local-daemon multi-round test for `quine-core` prompt-time recall

- **Goal**: Exercise the real CLI/IPC/harness/core flow across multiple turns and prove targeted recall affects a later turn while remaining ephemeral and deterministic.
- **Implementation requirement**: add one dedicated daemon-backed integration test using a deterministic local provider that exposes the exact outbound request messages and emits stable assistant text. The test must not depend on network LLM access.
- **Recommended test location**: `crates/quine-harness/tests/prompt_time_persistent_recall_daemon.rs`.
- **Exact command**:
  - `cargo test -p quine-harness prompt_time_persistent_recall_multi_round_local_daemon -- --exact --nocapture`
- **Exact daemon shape required inside the test**:
  - start the local IPC server against a temporary socket path
  - back it with a deterministic provider that returns a final assistant message derived from the latest request in a stable format, for example `observed-memory:<entry_ids>|last-user:<text>`
  - create the session through the harness JSON-RPC surface, not by mutating core state directly
- **Exact project setup required inside the test**:
  - create a temporary project root with a persistent-memory directory seeded with:
    - `MEMORY.md` containing an index row for three memory files
    - `entries/rust-test-command.md` with metadata/body stating `Use \`cargo test\` to run the Rust test suite.`
    - `entries/rust-build-command.md` with metadata/body stating `Use \`cargo build\` to compile the workspace.`
    - `entries/editor-preference.md` with unrelated preference text
  - enable prompt-memory `TargetedRecall` mode for the session using the implementation-supported config/test hook
- **Exact round-by-round chat messages**:
  - Round 1 user message: `Say exactly: round-1 acknowledged.`
  - Round 2 user message: `What command should I run to execute the Rust test suite? Answer with only the command.`
  - Round 3 user message: `What command should I run to build the workspace? Answer with only the command.`
- **Exact expected visible final responses with the deterministic provider**:
  - Round 1: exactly `observed-memory:|last-user:Say exactly: round-1 acknowledged.`
  - Round 2: exactly `observed-memory:rust-test-command|last-user:What command should I run to execute the Rust test suite? Answer with only the command.`
  - Round 3: exactly `observed-memory:rust-build-command|last-user:What command should I run to build the workspace? Answer with only the command.`
- **Exact expected status/tool/error behavior for every round**:
  - one `turn_complete` notification is emitted per round
  - no `session_error` notification is emitted
  - no tool request is emitted
  - no interaction request is emitted
- **Exact expected context/debug assertions after each round**:
  - after Round 1, `get_session_context` or equivalent additive snapshot shows no selected prompt-memory entries for the last turn, and `history` contains no synthetic reminder messages
  - after Round 2, the additive last-injection summary shows `TargetedRecall` with exactly `rust-test-command` selected, and `history` still contains no synthetic reminder messages
  - after Round 3, the additive last-injection summary shows `TargetedRecall` with exactly `rust-build-command` selected, and `history` still contains no synthetic reminder messages
- **Expected result**: the real daemon path proves later turns receive deterministic targeted recall, while storage/session history remain clean.

### 7. One-shot CLI/manual smoke for `IndexOnly` mode

- **Goal**: Give operators a replayable manual smoke path against a real local daemon once implementation lands.
- **Command sequence**:
  - Start a daemon in one terminal with an implementation-supported deterministic provider or local debug provider:
    - `cargo run --bin quine-harness -- start --socket /tmp/quine-recall.sock --state-dir /tmp/quine-recall-state`
  - In another terminal, run interactive chat and inspect context:
    - `cargo run --bin quine -- chat --socket /tmp/quine-recall.sock`
    - send: `/context`
    - send: `Summarize the currently loaded baseline memory in one sentence.`
    - send: `/context`
- **Expected result**:
  - session creation succeeds with no errors
  - the first `/context` output shows either the injected index text inside `system_prompt` or an additive prompt-memory summary indicating `IndexOnly`
  - the model reply is successful and non-empty
  - the second `/context` output still shows ordinary persisted history only; no targeted reminder messages have been appended into transcript history
- **Notes**:
  - this is supplementary smoke coverage only
  - exact response text is not required here because the production manual path may use a non-deterministic provider

## Required Evidence

- A mapping from each acceptance criterion to concrete tests or scenarios covering it:
  - baseline index injection ordering
  - deterministic relevant-memory ranking
  - truncation and total-budget handling
  - disabled-mode no-op behavior
  - prompt-construction integration with recall enabled
  - no shared inter-crate trait contract changes
- Output from the required daemon-backed test command:
  - `cargo test -p quine-harness prompt_time_persistent_recall_multi_round_local_daemon -- --exact --nocapture`
- Output from the focused targeted test commands listed above, or equivalent final test names if implementation chooses slightly different names while preserving the same coverage.
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
- Evidence from provider-request assertions or session-context snapshots proving:
  - `IndexOnly` includes the stale-memory caveat and bounded `MEMORY.md` content in deterministic order
  - `TargetedRecall` selects only the expected `entry_id`s for a given query and injects them before the newest user message
  - synthetic reminder messages do not persist into stored `history`
  - disabled mode emits no prompt-memory content
  - a no-match recall query injects nothing rather than falling back to broad index injection
- If additive session-context inspection is implemented, one captured JSON snippet showing the last prompt-memory summary for:
  - disabled or empty recall
  - successful targeted recall with selected entries
  - truncation/skipped-entry accounting

## Implementation Feedback

- I reviewed the current QA-plan skeleton against the implementation plan, and the highest-priority gap is still concreteness: because this feature changes `quine-core` prompt construction, the final QA plan needs at least one explicit multi-round local-daemon scenario with exact commands, exact user messages, and exact expected turn outputs.
- Please keep the QA scope tightly aligned to this feature slice only:
  - baseline `MEMORY.md` index injection
  - deterministic targeted recall from project-scoped persistent memory
  - budget/truncation behavior
  - disabled-mode no-op behavior
  - additive inspection of the last prompt-memory injection only if that surface lands
- Please do not expand QA coverage into deferred scopes such as team memory, agent-specific memory routing, rich operator UI, LLM-based retrieval, or broader memory diagnostics beyond the minimal additive session-context summary.
- The QA plan should explicitly cover both prompt paths described in the implementation plan:
  - `IndexOnly` uses bounded `MEMORY.md` content in the baseline prefix path with stale-memory caveat text
  - `TargetedRecall` injects ephemeral reminder messages before the newest user message and does not persist them into session history
- For the required local-daemon multi-round scenario, please make the setup deterministic and executable without inventing details:
  - state exactly how the daemon is started
  - state exactly how the test memory directory is prepared
  - state exactly how prompt-memory mode is enabled for the session
  - list the exact user messages round by round
  - list the exact expected response text or exact expected observable context/debug output per round
- Because prompt injection is internal, the QA plan should rely on one of two observable proofs, and specify which one each scenario uses:
  - deterministic response text from a controlled/in-process provider that echoes or exposes the injected prompt structure predictably
  - additive `get_session_context` / context-debug inspection showing the last prompt-memory mode and selected entries after a turn
- Please include a scenario that proves disabled mode is behaviorally unchanged, not just “memory not selected.” The scenario should show that with the feature disabled, no prompt-memory summary is recorded beyond a disabled/empty state and no memory-derived text appears in the request-visible outcome.
- Please include a scenario that proves targeted recall does not silently fall back to index injection when nothing matches the latest user message.
- Please include a scenario that proves deterministic ranking and truncation, ideally by preparing several durable memory entries whose relevance, pinned status, and recency make the expected winner order unambiguous.
- If the QA plan asks for inspection output, please keep it backward-compatible and summary-level. The implementation plan does not assume a new RPC method or a rich UI, only possible additive fields on the existing session-context snapshot.
- From the implementation side, there are no open questions left on scope or architecture. Once the QA plan is revised to include the concrete scenarios above, I can re-review it and update the implementation doc’s agreement status accordingly.
