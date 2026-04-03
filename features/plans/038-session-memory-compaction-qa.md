# 038 Session-Memory-Driven Compaction — QA Plan

Short summary: Verify that Quine compaction uses session-memory summary data and boundaries when valid, falls back safely when not, and preserves existing compaction invariants including transcript archives, live-tail handling, and tool-result archiving.

## Open Questions

- None. After reviewing `features/038-session-memory-compaction.md`, `features/plans/038-session-memory-compaction-implementation.md`, `docs/design/002-memory-systems-design.md`, `CLAUDE.md`, and the current `quine-core` compaction / archive flow, the remaining QA requirements are concrete and executable without additional clarification.

## Agreement Status

agreed — reviewed the latest implementation-plan revision after completing this QA plan; both docs now align on internal-only `quine-core` changes, preferred use of valid session-memory state, bounded coordination with in-flight refresh work, safe fallback to the legacy summarizer, and preservation of archive/live-tail/tool-result invariants, with no unresolved open questions.

## Test Strategy

- Validate the feature at four layers so failures localize cleanly:
  - focused `quine-core` unit tests for compaction-source selection, boundary validation, tail preservation, and fallback conditions
  - `quine-core` integration tests for end-to-end compaction behavior using temporary state roots and controllable provider behavior
  - at least one real local-daemon multi-round scenario that exercises the CLI → harness → core compaction path and inspects on-disk artifacts
  - workspace quality gates required by `CLAUDE.md`
- Keep QA scoped exactly to session-memory-driven compaction:
  - prove compaction prefers `session-memory/summary.md` plus boundary metadata when valid
  - prove compaction falls back to the existing summarizer when session-memory state is missing, malformed, stale, overlaps the protected live tail, or cannot be consumed consistently
  - prove no new broad external API contract is required beyond additive/internal behavior
- Treat the following as first-class observable contracts:
  - transcript archive JSON is still written before history replacement
  - the compacted history still preserves the system message, one compact assistant summary, and the live tail
  - archived tool results before the preserved tail remain archived/remapped exactly as today
  - manual and auto-compaction share the same summary-source selection logic
- Prefer evidence already observable from tests, checkpoint/session context, archive files, and session-memory files on disk. Do not require new operator-facing diagnostics to execute QA.
- For daemon scenarios, use exact commands, exact user messages, exact expected turn outputs, and exact inspected file locations so another agent can replay without inventing missing details.
- If the implementation adds persisted compaction metadata, require explicit backward-compatibility coverage proving older checkpoints still restore and safely use the legacy path.

## Scenarios

- **Scenario 1: Focused `quine-core` unit coverage for compaction source selection**
  - Start/use: run targeted unit tests in the compaction/session-memory modules after implementation lands.
  - Commands:
    - `cargo test -p quine-core session_memory_compaction_selection -- --nocapture`
    - `cargo test -p quine-core session_memory_compaction_boundary -- --nocapture`
    - `cargo test -p quine-core session_memory_compaction_fallback -- --nocapture`
  - Exact validation:
    - valid `summary.md` text plus valid boundary metadata inside the compactable prefix resolves to the `SessionMemory` source and does not invoke the legacy summarizer path
    - missing `summary.md`, template-only `summary.md`, missing/invalid metadata, stale boundary beyond current history length, and boundary overlap with the preserved live tail all resolve to `LegacySummarizer`
    - a boundary exactly at the last safe compactable message preserves every message after the boundary as the live unsummarized tail
    - a boundary that would require dropping any message in the protected tail is rejected in favor of fallback
  - Expected result: all targeted tests pass and isolate failures to the new source-selection and boundary-resolution logic.

- **Scenario 2: `quine-core` integration test uses session memory when valid**
  - Start/use: run the new integration test that seeds session-memory files and transcript history in a temporary harness state root, then triggers compaction.
  - Command: `cargo test -p quine-core valid_session_memory_compaction_uses_summary_md -- --nocapture`
  - Exact validation:
    - the test creates a transcript with a system message, several older turns, and a non-empty live tail
    - `summary.md` and its metadata sidecar identify a summarized boundary that ends before the protected tail begins
    - compaction writes a transcript archive file under the existing archive root before history replacement
    - the resulting compacted history begins with the original system message when present
    - the single compact assistant summary message contains the existing wrapper format from `compaction::compacted_history(...)` and uses body text derived from `summary.md`, not a newly generated legacy summary
    - every message after the recorded boundary remains present in order in the compacted history tail
    - older pre-boundary tool results remain archived/remapped using the existing placeholder/archive behavior
  - Expected result: valid session memory becomes the compact summary source while all pre-existing compaction invariants remain intact.

- **Scenario 3: `quine-core` integration test falls back safely when session memory is unusable**
  - Start/use: run the fallback-focused integration suite against a temp state root and controllable provider.
  - Commands:
    - `cargo test -p quine-core invalid_session_memory_compaction_falls_back -- --nocapture`
    - `cargo test -p quine-core stale_boundary_compaction_falls_back -- --nocapture`
    - `cargo test -p quine-core live_tail_overlap_compaction_falls_back -- --nocapture`
  - Exact validation:
    - compaction still succeeds when session-memory state is missing or unusable
    - the archive file is still emitted before replacement
    - the compact assistant summary is produced by the legacy summarizer path instead of `summary.md`
    - no message that would have been preserved by the old live-tail logic is lost
    - failures in session-memory loading/parsing do not surface as user-visible compaction failure if the legacy summarizer succeeds
  - Expected result: the session-memory path behaves as an optimization only; reliability matches the prior compaction path.

- **Scenario 4: Refresh-coordination integration coverage**
  - Start/use: run integration tests that simulate an in-flight session-memory refresh at compaction time.
  - Commands:
    - `cargo test -p quine-core compaction_waits_for_refresh_when_join_is_available -- --nocapture`
    - `cargo test -p quine-core compaction_falls_back_when_refresh_snapshot_is_not_safely_available -- --nocapture`
  - Exact validation:
    - when a concrete, cheap completion primitive is available, compaction waits only long enough to consume the completed snapshot and then uses `SessionMemory`
    - when no safe await path exists, the refresh errors, or the wait would exceed the bounded coordination policy, compaction chooses `LegacySummarizer`
    - neither case hangs indefinitely
    - neither case surfaces an avoidable session error when fallback remains available
  - Expected result: compaction consumes a consistent session-memory snapshot or degrades cleanly.

- **Scenario 5: Restore/backward-compatibility coverage for additive persisted metadata**
  - Start/use: run only if the implementation adds checkpointed compaction bookkeeping.
  - Command: `cargo test -p quine-core session_memory_compaction_restore_compat -- --nocapture`
  - Exact validation:
    - checkpoints written before this feature still restore successfully
    - restored sessions with no usable session-memory compaction metadata still compact through the legacy path
    - restored sessions with additive metadata do not require raw `summary.md` contents to be stored in the checkpoint payload
  - Expected result: additive persistence changes remain backward compatible.

- **Scenario 6: Manual local-daemon multi-round compaction flow**
  - Start/use: run against a real local daemon with an isolated state root and socket, then inspect on-disk state.
  - Setup commands:
    - `export QUINE_STATE_DIR="$(mktemp -d)"`
    - `export QUINE_SOCKET="$(mktemp -u /tmp/quine-session-memory-compaction-XXXXXX.sock)"`
    - `cargo run --bin quine-harness -- start --socket "$QUINE_SOCKET" --state-dir "$QUINE_STATE_DIR" > /tmp/quine-session-memory-compaction-daemon.log 2>&1 &`
    - `DAEMON_PID=$!`
    - `sleep 2`
  - Round-by-round chat commands and exact messages:
    - Round 1: `cargo run --bin quine -- run --json --socket "$QUINE_SOCKET" "Please remember this exact project brief for later compaction continuity: We are implementing session-memory-driven compaction in quine-core. Do not use any tools. Reply exactly with: ACK ROUND 1."`
    - Expected round 1 output:
      - stdout JSON has `response` exactly equal to `ACK ROUND 1.` or `ACK ROUND 1` only if the provider/test harness normalizes trailing punctuation consistently for the whole run; record the exact observed string once and require the same deterministic fixture thereafter
      - stdout JSON has `tool_calls` equal to `[]`
      - stderr contains `session: <SESSION_ID>` and save that `<SESSION_ID>` for the remaining steps
      - no `session error:` text appears
    - Round 2: `cargo run --bin quine -- run --json --socket "$QUINE_SOCKET" --session "$SESSION_ID" "Add this second continuity fact and again do not use tools: The preserved live tail must include any messages after the summarized boundary. Reply exactly with: ACK ROUND 2."`
    - Expected round 2 output:
      - stdout JSON `response` exactly matches `ACK ROUND 2.` or the same punctuation-normalized convention chosen in round 1
      - stdout JSON `tool_calls` is `[]`
      - no `session error:` text appears
    - Round 3: `cargo run --bin quine -- run --json --socket "$QUINE_SOCKET" --session "$SESSION_ID" "Add this final pre-compaction fact and do not use tools: If session memory is invalid, compaction must fall back safely. Reply exactly with: ACK ROUND 3."`
    - Expected round 3 output:
      - stdout JSON `response` exactly matches `ACK ROUND 3.` or the same chosen punctuation convention
      - stdout JSON `tool_calls` is `[]`
      - no `session error:` text appears
    - Manual compaction trigger: `printf '/compact\n/quit\n' | cargo run --bin quine -- chat --socket "$QUINE_SOCKET" --resume "$SESSION_ID"`
    - Expected compaction command output:
      - stderr contains `Session created: $SESSION_ID` or equivalent resumed-session banner for that same session id
      - output does not contain `Usage: /compact`
      - output does not contain `session error:`
      - the session remains usable after compaction completes
    - Post-compaction continuity check: `cargo run --bin quine -- run --json --socket "$QUINE_SOCKET" --session "$SESSION_ID" "Without using tools, list the three continuity facts I asked you to preserve before compaction as a numbered list with exactly three items."`
    - Expected continuity-check output:
      - stdout JSON `tool_calls` is `[]`
      - stdout JSON `response` contains all three facts in substance:
        - `session-memory-driven compaction in quine-core`
        - `preserved live tail must include any messages after the summarized boundary`
        - `If session memory is invalid, compaction must fall back safely`
      - no `session error:` text appears
  - Required artifact inspection commands:
    - `test -d "$QUINE_STATE_DIR/compactions/$SESSION_ID"`
    - `ls -1 "$QUINE_STATE_DIR/compactions/$SESSION_ID"`
    - `find "$QUINE_STATE_DIR" -path "*/$SESSION_ID/session-memory/*" -maxdepth 4 -type f | sort`
    - `python - <<'PY'
from pathlib import Path
import json, os
state = Path(os.environ['QUINE_STATE_DIR'])
sid = os.environ['SESSION_ID']
archives = sorted((state / 'compactions' / sid).glob('*.json'))
assert archives, 'missing compaction archive'
archive = json.loads(archives[-1].read_text())
assert archive['session_id'] == sid
assert len(archive['history']) >= 4
print(archives[-1])
PY`
  - Exact artifact validation:
    - at least one compaction archive exists for the session under `$QUINE_STATE_DIR/compactions/$SESSION_ID/`
    - session-memory files for the same session exist under the harness state root, not the repo working tree
    - the archive JSON still contains the full pre-compaction history
    - after compaction, the follow-up continuity question succeeds without requiring the earlier full transcript to remain verbatim in live history
  - Cleanup commands:
    - `kill "$DAEMON_PID" 2>/dev/null || true`
    - `wait "$DAEMON_PID" 2>/dev/null || true`

- **Scenario 7: Shared logic for manual and auto-compaction**
  - Start/use: run one targeted integration test or a pair of tests that trigger compaction once via explicit `CompactSession` and once via the auto-compaction threshold.
  - Commands:
    - `cargo test -p quine-core manual_compaction_uses_same_session_memory_selection_as_auto -- --nocapture`
  - Exact validation:
    - with the same transcript and the same session-memory state, both triggers choose the same compaction source
    - both triggers preserve the same tail boundary and archive behavior
  - Expected result: source selection is centralized rather than duplicated.

## Required Evidence

- Record the exact test commands executed and whether each passed.
- For every failed scenario, capture the failing command, stdout/stderr, and the specific invariant violated.
- For unit/integration coverage, retain evidence for these assertions:
  - which source was selected: `SessionMemory` vs `LegacySummarizer`
  - resolved boundary index and why it was accepted or rejected
  - preserved tail contents before and after compaction
  - transcript archive path and archive payload shape
  - tool-result archive/remap behavior before the preserved tail
- For the local-daemon scenario, retain:
  - the exact daemon-start command
  - the exact per-round user messages
  - the exact JSON outputs for rounds 1–3 and the post-compaction continuity check
  - the resumed `/compact` command transcript
  - the resolved session id and inspected filesystem paths
  - the archive JSON path created for the compaction event
- Run and record the workspace gates required by `CLAUDE.md` once implementation is ready:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
- If additive checkpoint metadata is introduced, retain one passing restore/backward-compatibility test result proving older checkpoints still restore.

## Exit Criteria

- All scenario commands are concrete enough to run without inventing missing setup, messages, or expectations.
- QA evidence proves both compaction source outcomes:
  - valid session-memory state selects `SessionMemory`
  - missing/invalid/stale/unsafe state selects `LegacySummarizer`
- QA evidence proves boundary safety and tail preservation:
  - pre-boundary content is compacted away from live history
  - protected live-tail content remains present and ordered
  - overlapping boundaries trigger fallback instead of message loss
- QA evidence proves existing compaction invariants still hold:
  - transcript archive emission still occurs before replacement
  - system-message preservation remains intact
  - archived tool-result handling remains intact
  - manual and auto-compaction share the same selection behavior
- QA evidence proves refresh coordination is bounded and safe:
  - compaction uses a completed refresh snapshot only when safely awaitable
  - otherwise compaction falls back without hanging or surfacing an avoidable error
- Workspace gates in `CLAUDE.md` pass, and no inter-crate trait change is required.

## Implementation Feedback

- Reviewed the latest implementation plan revision in `features/plans/038-session-memory-compaction-implementation.md`.
- The implementation scope is correct and appropriately constrained:
  - internal-only changes in `quine-core`
  - session-memory-driven compaction only
  - explicit preservation of the legacy summarizer fallback
  - no required shared inter-crate trait changes
- The QA plan should verify two distinct decision paths rather than only final compaction success:
  - `SessionMemory` path when `summary.md` and boundary metadata are both valid
  - `LegacySummarizer` path when summary state is missing, malformed, stale, or unsafe
- Please make the scenarios prove boundary safety, not just summary selection. Concretely, QA should check that:
  - messages before the resolved summarized boundary are removed from live history after compaction
  - messages after that boundary remain present as preserved tail
  - a boundary that overlaps the existing live-tail cutoff causes fallback instead of dropping messages
- Please include explicit regression coverage for current invariants the implementation is preserving:
  - transcript archive creation still happens before replacement
  - system-message preservation still holds
  - archived tool-result handling remains unchanged
  - manual and auto-compaction use the same selection logic
- Because this feature changes `quine-core` compaction behavior, the QA plan should include at least one concrete daemon-backed multi-round scenario with exact commands and messages. That scenario should exercise enough turns to create session memory, then trigger compaction and inspect on-disk artifacts and resulting session behavior.
- For refresh coordination, the QA plan should include one scenario or test case for each expected outcome:
  - refresh completes in time and compaction uses the resulting session-memory snapshot
  - refresh cannot be consumed consistently, so compaction falls back without hanging or surfacing an avoidable error
- For observability, prefer evidence already available from:
  - transcript archive files under the existing archive root
  - session-memory files / metadata on disk
  - restored or compacted session history in tests
  - existing logs, if stable
  rather than depending on new operator-facing diagnostics.
- If the implementation ends up making persisted compaction metadata additive in checkpoints, the QA plan should also require backward-compatibility coverage proving older checkpoints still restore and safely use legacy compaction when session-memory state is not yet usable.
- No blocking design issues remain from implementation review. Once the QA plan is filled in with concrete executable scenarios reflecting the points above, this doc can move toward agreement.
